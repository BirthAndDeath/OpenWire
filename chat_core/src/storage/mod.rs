mod contact;
mod identity;
mod message;
mod migrations;
mod stats;

pub use contact::{Contact, delete_contact, get_contact_public_key, is_contact_exists, list_contacts, upsert_contact};
pub use identity::{
    MlKemIdentity, add_mlkem_identity, delete_mlkem_identity, get_current_mlkem_identity, get_current_mlkem_public_key,
    init, init_path, list_mlkem_identities, pool,
    set_current_mlkem_identity,
};
pub use message::{
    Message, add_message, add_messages_batch, delete_message, delete_messages_batch,
    get_last_message, get_message, get_messages, list_failed, list_pending, mark_failed,
    mark_pending, mark_sent, mark_sent_batch,
};
