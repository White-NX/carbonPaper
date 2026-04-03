use crate::credential_manager::CredentialManagerState;

/// Checks whether the current session requires re-authentication.
pub fn check_auth_required(credential_state: &CredentialManagerState) -> Result<(), String> {
    if !credential_state.is_session_valid() {
        return Err("AUTH_REQUIRED".to_string());
    }
    Ok(())
}

pub mod credential;
pub mod mcp;
pub mod migration;
pub mod storage;
pub mod utility;
