use async_trait::async_trait;
use futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::{
    StreamProtocol,
    request_response::{self, Codec, ProtocolSupport},
    swarm::NetworkBehaviour,
};
use serde::{Deserialize, Serialize};
use std::io;
use tracing;

pub const PROTOCOL_NAME: StreamProtocol = StreamProtocol::new("/pathranker/0.1.0");

// ---------- 子模块 ----------
pub mod node;
pub mod ranker;

// ---------- 消息定义 ----------
/// 查询某个目标节点的评分。
/// `nonce` 用于防重放攻击，每次请求随机生成。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRequest {
    pub target: String,
    pub nonce: u64,
}

/// 返回的评分信息（双维评分模型：他评 + 自评）。
///
/// 字段说明：
/// - `score`：对目标节点的历史表现评分（EWMA 延迟 + 成功率），范围 [0,1]
/// - `self_score`：发起响应节点自身的负载评分，范围 [0,1]，1 = 完全空闲
/// - `overloaded`：简化的过载标记，当 `self_score < 0.3` 时为 true
/// - `nonce`：回显请求中的 nonce，用于防重放
/// - `signature`：Ed25519 签名，覆盖 `(responder || score || updated_at || target || nonce || self_score)`
/// - `responder_key`：protobuf 编码的公钥，接收方用于验签
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResponse {
    pub score: f64,
    pub updated_at: u64,
    pub nonce: u64,
    pub target: String,
    pub signature: Vec<u8>,
    pub responder_key: Vec<u8>,
    pub overloaded: bool,
    pub self_score: f64,
}

// ---------- 编解码器 ----------
/// 自定义的请求/响应编解码器。
#[derive(Debug, Clone, Default)]
pub struct PathRankerCodec;

#[async_trait]
impl Codec for PathRankerCodec {
    type Protocol = StreamProtocol;
    type Request = ScoreRequest;
    type Response = ScoreResponse;

    async fn read_request<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let _ = protocol;
        let mut len_buf = [0u8; 4];
        io.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf);
        let mut buf = vec![0u8; len as usize];
        io.read_exact(&mut buf).await?;
        serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    async fn read_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let _ = protocol;
        let mut len_buf = [0u8; 4];
        io.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf);
        let mut buf = vec![0u8; len as usize];
        io.read_exact(&mut buf).await?;
        serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    async fn write_request<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        let _ = protocol;
        let data =
            serde_json::to_vec(&req).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        io.write_all(&(data.len() as u32).to_be_bytes()).await?;
        io.write_all(&data).await?;
        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        resp: Self::Response,
    ) -> io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        let _ = protocol;
        let data =
            serde_json::to_vec(&resp).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        io.write_all(&(data.len() as u32).to_be_bytes()).await?;
        io.write_all(&data).await?;
        Ok(())
    }
}

// ---------- 行为定义 ----------
/// 封装 `request_response::Behaviour`，提供基于 `/pathranker/0.1.0` 协议的评分查询能力。
///
/// 集成 Ed25519 签名，确保响应不可伪造、不可篡改。
/// 签名覆盖 `(responder || score || updated_at || target || nonce || self_score)`。
///
/// 手写 `impl NetworkBehaviour`（libp2p 0.56 的 derive 宏不支持非 Behaviour 字段）。
pub struct PathRankerBehaviour {
    inner: request_response::Behaviour<PathRankerCodec>,
    local_key: libp2p::identity::Keypair,
}

impl PathRankerBehaviour {
    pub fn new(local_key: libp2p::identity::Keypair) -> Self {
        let rr = request_response::Behaviour::new(
            [(PROTOCOL_NAME, ProtocolSupport::Full)],
            request_response::Config::default(),
        );
        Self { inner: rr, local_key }
    }

    pub fn local_peer_id(&self) -> libp2p::PeerId {
        self.local_key.public().to_peer_id()
    }

    /// 向指定节点发送评分查询。自动生成随机 nonce 防重放。
    pub fn send_query(&mut self, peer: &libp2p::PeerId, target: String) -> libp2p::request_response::OutboundRequestId {
        self.inner.send_request(peer, ScoreRequest { target, nonce: rand::random() })
    }

    /// 回复评分查询。自动填充 `responder_key` 和 `signature`。
    /// 签名失败时返回 `Err(resp)`，调用方可选择如何处理。
    pub fn send_response(
        &mut self,
        channel: request_response::ResponseChannel<ScoreResponse>,
        mut resp: ScoreResponse,
    ) -> Result<(), ScoreResponse> {
        let public_key = self.local_key.public();
        resp.responder_key = public_key.encode_protobuf();
        let responder_id = public_key.to_peer_id();
        let payload = Self::build_signing_payload(&responder_id, resp.score, resp.updated_at, &resp.target, resp.nonce, resp.self_score);
        match self.local_key.sign(&payload) {
            Ok(sig) => {
                resp.signature = sig;
                self.inner.send_response(channel, resp)
            }
            Err(e) => {
                tracing::error!("failed to sign ScoreResponse: {:?}", e);
                Err(resp)
            }
        }
    }

    /// 验签：从 `responder_key` 解码公钥，重构签名载荷并验证。
    /// 返回 `false` 表示签名无效或公钥解码失败。
    pub fn verify_response(resp: &ScoreResponse) -> bool {
        let pk = match libp2p::identity::PublicKey::try_decode_protobuf(&resp.responder_key) {
            Ok(pk) => pk,
            Err(_) => return false,
        };
        let responder_id = pk.to_peer_id();
        let payload = Self::build_signing_payload(&responder_id, resp.score, resp.updated_at, &resp.target, resp.nonce, resp.self_score);
        pk.verify(&payload, &resp.signature)
    }

    /// 构建签名载荷，所有变长字段前加 4 字节大端长度前缀，防止域碰撞。
    fn build_signing_payload(responder: &libp2p::PeerId, score: f64, updated_at: u64, target: &str, nonce: u64, self_score: f64) -> Vec<u8> {
        let mut data = Vec::new();
        let responder_bytes = responder.to_bytes();
        data.extend_from_slice(&(responder_bytes.len() as u32).to_be_bytes());
        data.extend_from_slice(&responder_bytes);
        data.extend_from_slice(&score.to_be_bytes());
        data.extend_from_slice(&updated_at.to_be_bytes());
        data.extend_from_slice(&(target.len() as u32).to_be_bytes());
        data.extend_from_slice(target.as_bytes());
        data.extend_from_slice(&nonce.to_be_bytes());
        data.extend_from_slice(&self_score.to_be_bytes());
        data
    }
}

/// 方便外部匹配的事件类型
pub type PathRankerEvent = request_response::Event<ScoreRequest, ScoreResponse>;

impl NetworkBehaviour for PathRankerBehaviour {
    type ConnectionHandler =
        <request_response::Behaviour<PathRankerCodec> as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = request_response::Event<ScoreRequest, ScoreResponse>;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: libp2p::swarm::ConnectionId,
        peer: libp2p::PeerId,
        local_addr: &libp2p::Multiaddr,
        remote_addr: &libp2p::Multiaddr,
    ) -> Result<Self::ConnectionHandler, libp2p::swarm::ConnectionDenied> {
        self.inner.handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: libp2p::swarm::ConnectionId,
        peer: libp2p::PeerId,
        addr: &libp2p::Multiaddr,
        role_override: libp2p::core::Endpoint,
        port_use: libp2p::core::transport::PortUse,
    ) -> Result<Self::ConnectionHandler, libp2p::swarm::ConnectionDenied> {
        self.inner.handle_established_outbound_connection(connection_id, peer, addr, role_override, port_use)
    }

    fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm) {
        self.inner.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: libp2p::PeerId,
        connection_id: libp2p::swarm::ConnectionId,
        event: libp2p::swarm::THandlerOutEvent<Self>,
    ) {
        self.inner.on_connection_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>> {
        self.inner.poll(cx)
    }
}

// ---------- 核心评分器接口 ----------
/// 行为感知路由的核心 trait，定义了路径排序和反馈接口。
///
/// 当前由 `BehaviorRanker` 实现，直接调用其固有方法即可。
/// 此 trait 保留用于泛型编程场景——若不需要，可安全删除。
pub trait PathRanker {
    /// 对候选地址列表排序，返回按评分降序的地址序列
    fn rank(&self, peer: &libp2p::PeerId, addrs: Vec<libp2p::Multiaddr>) -> Vec<libp2p::Multiaddr>;

    /// 通信结束后反馈实际表现，更新内部评分
    fn feedback(
        &mut self,
        peer: &libp2p::PeerId,
        addr: &libp2p::Multiaddr,
        latency: std::time::Duration,
        success: bool,
    );
}

#[cfg(test)]
mod codec_tests {
    use super::*;

    #[test]
    fn test_score_request_roundtrip() {
        let req = ScoreRequest { target: "12D3KooWAbcd".into(), nonce: 42 };
        let json = serde_json::to_vec(&req).unwrap();
        let decoded: ScoreRequest = serde_json::from_slice(&json).unwrap();
        assert_eq!(req.target, decoded.target);
        assert_eq!(req.nonce, decoded.nonce);
    }

    #[test]
    fn test_score_response_roundtrip() {
        let resp = ScoreResponse {
            score: 0.85,
            updated_at: 1000,
            nonce: 42,
            target: "12D3KooWAbcd".into(),
            signature: vec![1, 2, 3],
            responder_key: vec![4, 5, 6],
            overloaded: false,
            self_score: 0.9,
        };
        let json = serde_json::to_vec(&resp).unwrap();
        let decoded: ScoreResponse = serde_json::from_slice(&json).unwrap();
        assert!((decoded.score - 0.85).abs() < 1e-10);
        assert_eq!(decoded.nonce, 42);
        assert_eq!(decoded.target, "12D3KooWAbcd");
        assert_eq!(decoded.signature, vec![1, 2, 3]);
        assert!(!decoded.overloaded);
        assert!((decoded.self_score - 0.9).abs() < 1e-10);
    }
}
