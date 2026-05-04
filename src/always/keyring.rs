//! Keychain/keyring integration for secure API key storage
//!
//! Provides secure storage for API keys using platform-specific credential stores:
//! - macOS: Keychain
//! - Linux: secret-service
//! - Windows: Windows Credential Manager

use anyhow::{Context, Result};
use keyring::{Entry, Error as KeyringError};

const SERVICE_NAME: &str = "com.always.daemon";
const GROQ_API_KEY_ACCOUNT: &str = "groq_api_key";
const DEEPGRAM_API_KEY_ACCOUNT: &str = "deepgram_api_key";

/// Get the Groq API key from keyring or environment variable
pub fn get_groq_api_key() -> Result<Option<String>> {
    // Try environment variable first (highest priority)
    if let Ok(key) = std::env::var("GROQ_API_KEY") {
        return Ok(Some(key));
    }

    // Try keyring
    match get_secret(GROQ_API_KEY_ACCOUNT) {
        Ok(Some(key)) => Ok(Some(key)),
        Ok(None) => Ok(None),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to read Groq API key from keyring");
            Ok(None)
        }
    }
}

/// Set the Groq API key in keyring
pub fn set_groq_api_key(key: &str) -> Result<()> {
    set_secret(GROQ_API_KEY_ACCOUNT, key)
}

/// Delete the Groq API key from keyring
pub fn delete_groq_api_key() -> Result<()> {
    delete_secret(GROQ_API_KEY_ACCOUNT)
}

/// Get the Deepgram API key from keyring
pub fn get_deepgram_api_key() -> Result<Option<String>> {
    match get_secret(DEEPGRAM_API_KEY_ACCOUNT) {
        Ok(Some(key)) => Ok(Some(key)),
        Ok(None) => Ok(None),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to read Deepgram API key from keyring");
            Ok(None)
        }
    }
}

/// Set the Deepgram API key in keyring
pub fn set_deepgram_api_key(key: &str) -> Result<()> {
    set_secret(DEEPGRAM_API_KEY_ACCOUNT, key)
}

/// Delete the Deepgram API key from keyring
pub fn delete_deepgram_api_key() -> Result<()> {
    delete_secret(DEEPGRAM_API_KEY_ACCOUNT)
}

/// Get a secret from keyring
fn get_secret(account: &str) -> Result<Option<String>> {
    let entry = Entry::new(SERVICE_NAME, account)?;

    match entry.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(e) => Err(e).context("Failed to read secret from keyring"),
    }
}

/// Set a secret in keyring
fn set_secret(account: &str, secret: &str) -> Result<()> {
    let entry = Entry::new(SERVICE_NAME, account)?;
    entry.set_password(secret)?;
    tracing::info!(account, "Secret stored in keyring");
    Ok(())
}

/// Delete a secret from keyring
fn delete_secret(account: &str) -> Result<()> {
    let entry = Entry::new(SERVICE_NAME, account)?;
    // Try to delete by setting to empty string (keyring 2.3 doesn't have delete_credential)
    match entry.set_password("") {
        Ok(()) => tracing::info!(account, "Secret deleted from keyring"),
        Err(KeyringError::NoEntry) => tracing::debug!(account, "Secret not found in keyring"),
        Err(e) => return Err(e).context("Failed to delete secret from keyring"),
    }
    Ok(())
}

/// Migrate API keys from SQLite to keyring
///
/// This should be called once during upgrade to move keys from the database
/// to the secure keyring. After migration, the database fields should be nulled out.
pub fn migrate_keys_from_db() -> Result<()> {
    use crate::db;

    let conn = db::open()?;
    let prefs = db::get_preferences(&conn)?;

    let mut migrated = false;

    // Migrate Groq API key
    if let Some(groq_key) = prefs.groq_api_key.as_ref()
        && get_groq_api_key()?.is_none()
    {
        set_groq_api_key(groq_key)?;
        // Null out in database
        db::set_preference(&conn, "groq_api_key", "")?;
        migrated = true;
    }

    // Migrate Deepgram API key
    if let Some(deepgram_key) = prefs.deepgram_api_key.as_ref()
        && get_deepgram_api_key()?.is_none()
    {
        set_deepgram_api_key(deepgram_key)?;
        // Null out in database
        db::set_preference(&conn, "deepgram_api_key", "")?;
        migrated = true;
    }

    if migrated {
        tracing::info!("API keys migrated from database to keyring");
    }

    Ok(())
}
