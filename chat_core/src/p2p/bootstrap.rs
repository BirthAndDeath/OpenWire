use libp2p::Multiaddr;

///公共 bootstrap 节点
pub const BOOTSTRAP: &[(&str, &str)] = &[
    // (PeerId, Multiaddr)
    (
        "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
        "/dnsaddr/sv15.bootstrap.libp2p.io",
    ),
    (
        "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
        "/dnsaddr/ny5.bootstrap.libp2p.io",
    ),
    (
        "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
        "/dnsaddr/am6.bootstrap.libp2p.io",
    ),
    (
        "QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
        "/dnsaddr/sg1.bootstrap.libp2p.io",
    ),
    /*(
        "QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
        "/ip4/104.131.131.82/tcp/4001",
    ),
    (
        "QmSoLnSGccFuZQJzRadHn95W2CrSFmZuTdDWP8HXaHca9z",
        "/ip4/104.236.176.52/tcp/4001",
    ),
    (
        "QmSoLPppuBtQSGwKDZT2M73ULpjvfd3aZ6ha4oFGL1KrGM",
        "/ip4/104.236.179.241/tcp/4001",
    ),
    (
        "QmSoLueR4xBeUbY9WZ9xGUUxunbKWcrNFTDAadQJmocnWm",
        "/ip4/162.243.248.213/tcp/4001",
    ),
    (
        "QmSoLSafTMBsPKadTEgaXctDQVcqN88CNLHXMkTNwMKPnu",
        "/ip4/128.199.219.111/tcp/4001",
    ),
    (
        "QmSoLV4Bbm51jM9C4gDYZQ9Cy3U6aXMJDAbzgu2fzaDs64",
        "/ip4/104.236.76.40/tcp/4001",
    ),
    (
        "QmSoLer265NRgSp2LA3dPaeykiS1J6DifTC88f5uVQKNAd",
        "/ip4/178.62.158.247/tcp/4001",
    ),
    (
        "QmSoLMeWqB7YGVLJN3pNLQpmmEk35v6wYtsMGLzSr5QBU3",
        "/ip4/178.62.61.185/tcp/4001",
    ),
    (
        "QmSoLju6m7xTh3DuokvT3886QRYqxAzb1kShaanJgW36yx",
        "/ip4/104.236.151.122/tcp/4001",
    ), */
];

/// 解析 dnsaddr 为实际地址
pub fn resolve_dnsaddr(dnsaddr: &str) -> Multiaddr {
    dnsaddr.parse().unwrap()
}
