#[cfg(not(test))]
const PACKAGE: &str = "com.treitforge.qsoripper";
#[cfg(not(test))]
const SERVICE: &str = "qrz";
#[cfg(all(not(test), target_os = "macos"))]
const FULL_SERVICE: &str = "com.treitforge.qsoripper.qrz";

#[derive(Clone, Copy, Debug)]
pub(crate) enum QrzSecret {
    XmlPassword,
    LogbookApiKey,
}

impl QrzSecret {
    fn account(self) -> &'static str {
        match self {
            Self::XmlPassword => "xml-password",
            Self::LogbookApiKey => "logbook-api-key",
        }
    }
}

pub(crate) trait QrzSecretStore: Send + Sync {
    fn get(&self, secret: QrzSecret) -> Result<Option<String>, String>;
    fn set(&self, secret: QrzSecret, value: &str) -> Result<(), String>;
}

#[cfg(not(test))]
pub(crate) struct PlatformQrzSecretStore;

#[cfg(not(test))]
impl PlatformQrzSecretStore {
    pub(crate) fn new() -> Self {
        Self
    }

    fn entry(secret: QrzSecret) -> Result<keyring::Entry, String> {
        let account = secret.account();

        #[cfg(target_os = "windows")]
        let entry = keyring::Entry::new(&format!("{SERVICE}/{account}"), PACKAGE);

        #[cfg(target_os = "macos")]
        let entry = keyring::Entry::new(FULL_SERVICE, account);

        #[cfg(all(unix, not(target_os = "macos")))]
        let entry = keyring::Entry::new(SERVICE, account);

        #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
        return Err("The platform credential store is not supported.".to_string());

        entry.map_err(|error| format!("Platform credential store is unavailable: {error}"))
    }
}

#[cfg(not(test))]
impl QrzSecretStore for PlatformQrzSecretStore {
    fn get(&self, secret: QrzSecret) -> Result<Option<String>, String> {
        match Self::entry(secret)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(format!(
                "Failed to read the platform credential store: {error}"
            )),
        }
    }

    fn set(&self, secret: QrzSecret, value: &str) -> Result<(), String> {
        let entry = Self::entry(secret)?;
        entry
            .set_password(value)
            .map_err(|error| format!("Failed to write the platform credential store: {error}"))?;

        #[cfg(all(unix, not(target_os = "macos")))]
        entry
            .inner
            .update_attributes(&std::collections::HashMap::from([("xdg:schema", PACKAGE)]))
            .map_err(|error| {
                format!("Failed to set the platform credential store schema: {error}")
            })?;

        Ok(())
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemoryQrzSecretStore {
    values: std::sync::RwLock<std::collections::HashMap<&'static str, String>>,
}

#[cfg(test)]
impl QrzSecretStore for MemoryQrzSecretStore {
    fn get(&self, secret: QrzSecret) -> Result<Option<String>, String> {
        let values = self
            .values
            .read()
            .map_err(|_| "Memory credential store read failed.".to_string())?;
        Ok(values.get(secret.account()).cloned())
    }

    fn set(&self, secret: QrzSecret, value: &str) -> Result<(), String> {
        let mut values = self
            .values
            .write()
            .map_err(|_| "Memory credential store write failed.".to_string())?;
        values.insert(secret.account(), value.to_string());
        Ok(())
    }
}
