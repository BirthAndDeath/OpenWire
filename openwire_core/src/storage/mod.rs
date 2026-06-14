mod contact;
mod identity;
mod message;
mod migrations;
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
    delete_messages_batch, delete_messages_by_peer, get_last_message, get_message, get_messages,
    list_failed, list_pending, mark_failed, mark_pending, mark_sent, mark_sent_batch,
    mark_sent_by_hash, update_message_hash,
};
