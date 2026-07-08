/// 联系人管理模块
mod contact;
/// 身份管理模块
mod identity;
/// 消息管理模块
mod message;
/// 数据库迁移模块
mod migrations;
/// 已发送文件历史模块
mod sent_file;
/// 统计查询模块
mod stats;

pub use contact::{
    Contact, clear_all_mlkem_pubkeys, delete_contact, get_contact_by_mldsa_pubkey,
    get_contact_mlkem_pubkey, is_contact_exists, list_contacts, update_contact_mlkem_pubkey,
    upsert_contact,
};
pub use identity::{
    Identity, add_identity, delete_identity, get_current_identity, init, init_path,
    list_identities, pool, set_current_identity,
};
pub use message::{
    Message, add_message, add_message_with_hash, add_messages_batch, delete_message,
    delete_messages_batch, delete_messages_by_peer, get_last_message, get_message,
    get_message_by_hash, get_messages, get_messages_range, list_failed, list_pending,
    list_pending_by_peer, mark_failed, mark_pending, mark_sent, mark_sent_batch,
    mark_sent_by_hash, update_message_hash,
};
pub use sent_file::{add_sent_file, get_sent_file};