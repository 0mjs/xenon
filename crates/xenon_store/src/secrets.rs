//! Passwords, kept in the macOS Keychain (service "Xenon"), never on disk.

use anyhow::Result;

const SERVICE: &str = "Xenon";

#[cfg(target_os = "macos")]
mod platform {
    use anyhow::{Context as _, Result};
    use security_framework::passwords;

    /// errSecItemNotFound
    const NOT_FOUND: i32 = -25300;

    pub fn set(service: &str, key: &str, secret: &str) -> Result<()> {
        passwords::set_generic_password(service, key, secret.as_bytes())
            .context("could not save the password to the Keychain")
    }

    pub fn get(service: &str, key: &str) -> Result<Option<String>> {
        match passwords::get_generic_password(service, key) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(err) if err.code() == NOT_FOUND => Ok(None),
            Err(err) => Err(err).context("could not read the password from the Keychain"),
        }
    }

    pub fn delete(service: &str, key: &str) -> Result<()> {
        match passwords::delete_generic_password(service, key) {
            Err(err) if err.code() != NOT_FOUND => {
                Err(err).context("could not remove the password from the Keychain")
            }
            _ => Ok(()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use anyhow::{Result, bail};

    pub fn set(_: &str, _: &str, _: &str) -> Result<()> {
        bail!("saving passwords isn't supported on this platform yet")
    }

    pub fn get(_: &str, _: &str) -> Result<Option<String>> {
        Ok(None)
    }

    pub fn delete(_: &str, _: &str) -> Result<()> {
        Ok(())
    }
}

/// Store `secret` under `key`, replacing any previous value. An empty secret
/// removes the entry.
pub fn set(key: &str, secret: &str) -> Result<()> {
    if secret.is_empty() {
        return delete(key);
    }
    platform::set(SERVICE, key, secret)
}

pub fn get(key: &str) -> Result<Option<String>> {
    platform::get(SERVICE, key)
}

pub fn delete(key: &str) -> Result<()> {
    platform::delete(SERVICE, key)
}

#[cfg(test)]
mod tests {
    /// Touches the real login Keychain, so it only runs on request:
    /// `cargo test -p xenon_store -- --ignored`
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        let key = "xenon-self-test";
        super::set(key, "s3cret").unwrap();
        assert_eq!(super::get(key).unwrap().as_deref(), Some("s3cret"));
        super::set(key, "").unwrap();
        assert_eq!(super::get(key).unwrap(), None);
        super::delete(key).unwrap();
    }
}
