//! Desktop Secret Service storage, shared by GNOME Keyring and KWallet.
use crate::VaultError;
use secret_service::{blocking::SecretService, EncryptionType};
use std::collections::HashMap;
use zeroize::Zeroizing;

fn connect() -> Result<SecretService<'static>, VaultError> {
    SecretService::connect(EncryptionType::Dh).map_err(|_| VaultError::Inaccessible)
}
fn attributes(service: &str) -> Result<HashMap<&str, &str>, VaultError> {
    if service.is_empty() || service.len() > 512 || service.contains('\0') {
        return Err(VaultError::Invalid);
    }
    Ok(HashMap::from([
        ("application", "app.zephium"),
        ("service", service),
        ("account", "zephium"),
    ]))
}
/// Copies one exact credential into zeroizing storage.
pub fn read(service: &str) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    let service_api = connect()?;
    let found = service_api
        .search_items(attributes(service)?)
        .map_err(|_| VaultError::Inaccessible)?;
    let item = found
        .unlocked
        .first()
        .or_else(|| found.locked.first())
        .ok_or(VaultError::Missing)?;
    item.unlock().map_err(|_| VaultError::Inaccessible)?;
    let bytes = Zeroizing::new(item.get_secret().map_err(|_| VaultError::Inaccessible)?);
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err(VaultError::Invalid);
    }
    Ok(bytes)
}
/// Checks matching attributes without fetching secret bytes or unlocking.
pub fn present(service: &str) -> Result<bool, VaultError> {
    let service_api = connect()?;
    let found = service_api
        .search_items(attributes(service)?)
        .map_err(|_| VaultError::Inaccessible)?;
    Ok(!found.unlocked.is_empty() || !found.locked.is_empty())
}
/// Replaces only the exact application, service and account tuple.
pub fn write(service: &str, bytes: &[u8]) -> Result<(), VaultError> {
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err(VaultError::Invalid);
    }
    let service_api = connect()?;
    let collection = service_api
        .get_default_collection()
        .map_err(|_| VaultError::Inaccessible)?;
    collection
        .ensure_unlocked()
        .map_err(|_| VaultError::Inaccessible)?;
    collection
        .create_item(
            &format!("Zephium — {service}"),
            attributes(service)?,
            bytes,
            true,
            "application/octet-stream",
        )
        .map_err(|_| VaultError::Inaccessible)?;
    Ok(())
}
/// Deletes only the exact tuple. A missing credential is already deleted.
pub fn delete(service: &str) -> Result<(), VaultError> {
    let service_api = connect()?;
    let found = service_api
        .search_items(attributes(service)?)
        .map_err(|_| VaultError::Inaccessible)?;
    for item in found.unlocked.iter().chain(found.locked.iter()) {
        item.delete().map_err(|_| VaultError::Inaccessible)?;
    }
    Ok(())
}

/// Lists only Zephium service names matching a prefix, without reading secrets.
pub fn list(prefix: &str) -> Result<Vec<String>, VaultError> {
    attributes(prefix)?;
    let service_api = connect()?;
    let found = service_api
        .search_items(HashMap::from([
            ("application", "app.zephium"),
            ("account", "zephium"),
        ]))
        .map_err(|_| VaultError::Inaccessible)?;
    if found.unlocked.len().saturating_add(found.locked.len()) > 4096 {
        return Err(VaultError::Capacity);
    }
    let mut names = Vec::new();
    for item in found.unlocked.iter().chain(found.locked.iter()) {
        let metadata = item
            .get_attributes()
            .map_err(|_| VaultError::Inaccessible)?;
        if let Some(service) = metadata
            .get("service")
            .filter(|s| s.starts_with(prefix) && s.len() <= 512)
        {
            names.push(service.clone());
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_the_exact_secret_namespace() {
        assert!(attributes("").is_err());
        assert!(attributes("x\0y").is_err());
        assert_eq!(
            attributes("test").unwrap().get("application"),
            Some(&"app.zephium")
        );
    }
    #[test]
    #[ignore = "requires a running unlocked desktop Secret Service"]
    fn desktop_secret_round_trip() {
        let target = format!("app.zephium.test.{}", std::process::id());
        assert!(!present(&target).unwrap());
        write(&target, b"test-only-secret").unwrap();
        assert!(present(&target).unwrap());
        assert_eq!(&**read(&target).unwrap(), b"test-only-secret");
        write(&target, b"replacement").unwrap();
        assert_eq!(&**read(&target).unwrap(), b"replacement");
        delete(&target).unwrap();
        assert!(!present(&target).unwrap());
        assert_eq!(read(&target), Err(VaultError::Missing));
    }
}
