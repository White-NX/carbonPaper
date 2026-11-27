//! Password-protected portable backup streams compiled in the optimized core.

pub(super) use carbonpaper_backup_core::{
    check_cancelled, EncryptedReader, EncryptedWriter, MAGIC,
};
