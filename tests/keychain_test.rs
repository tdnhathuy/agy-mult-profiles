use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[path = "../src/keychain.rs"]
mod keychain;
#[path = "../src/security.rs"]
mod security;
use keychain::{
    decode_jwt_payload, extract_email_from_token_string, load_token_from_profile,
    save_token_to_profile, KeychainBackend, KeychainGuard,
};

#[test]
fn test_decode_jwt_payload() {
    let payload_json = json!({
        "email": "developer@example.com",
        "sub": "1234567890",
        "email_verified": true
    })
    .to_string();

    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(&payload_json);
    let jwt = format!("{}.{}.fake_signature", header, payload);

    let decoded = decode_jwt_payload(&jwt).expect("should decode JWT payload");
    assert_eq!(decoded["email"], "developer@example.com");
    assert_eq!(decoded["sub"], "1234567890");
}

#[test]
fn test_extract_email_from_token_string_go_keyring() {
    let payload_json = json!({
        "email": "alice@gmail.com"
    })
    .to_string();
    let jwt = format!("header.{}.sig", URL_SAFE_NO_PAD.encode(&payload_json));

    let token_obj = json!({
        "auth_method": "consumer",
        "id_token": jwt,
        "token": {
            "access_token": "ya29.dummy",
            "token_type": "Bearer"
        }
    });

    let raw_b64 = STANDARD.encode(token_obj.to_string());
    let keyring_str = format!("go-keyring-base64:{}", raw_b64);

    let email = extract_email_from_token_string(&keyring_str);
    assert_eq!(email, Some("alice@gmail.com".to_string()));
}

#[test]
fn test_extract_email_from_nested_token_json() {
    let payload_json = json!({
        "email": "bob@gmail.com"
    })
    .to_string();
    let jwt = format!("header.{}.sig", URL_SAFE_NO_PAD.encode(&payload_json));

    let token_obj = json!({
        "token": {
            "id_token": jwt,
            "access_token": "ya29.dummy"
        }
    });

    let email = extract_email_from_token_string(&token_obj.to_string());
    assert_eq!(email, Some("bob@gmail.com".to_string()));
}

#[derive(Clone)]
struct AmbiguousActivationBackend {
    token: Arc<Mutex<Option<String>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl KeychainBackend for AmbiguousActivationBackend {
    fn read_token(&self) -> Result<Option<String>, String> {
        self.calls.lock().unwrap().push("read".to_string());
        Ok(self.token.lock().unwrap().clone())
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("write:{token}"));
        *self.token.lock().unwrap() = Some(token.to_string());
        Ok(())
    }

    fn delete_token(&self) -> Result<(), String> {
        self.calls.lock().unwrap().push("delete".to_string());
        *self.token.lock().unwrap() = None;
        Ok(())
    }

    fn write_token_until(&self, token: &str, _deadline: Instant) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("write_until:{token}"));
        *self.token.lock().unwrap() = Some(token.to_string());
        Err("activation result became uncertain after mutation".to_string())
    }
}

#[test]
fn test_keychain_activation_failure_restores_snapshot() {
    let profile = tempdir().unwrap();
    save_token_to_profile(profile.path(), "target-token").unwrap();
    let backend = AmbiguousActivationBackend {
        token: Arc::new(Mutex::new(Some("original-token".to_string()))),
        calls: Arc::new(Mutex::new(Vec::new())),
    };

    let result = KeychainGuard::new_with_backend_until(
        profile.path(),
        backend.clone(),
        Instant::now() + Duration::from_secs(1),
    );

    assert!(result.is_err());
    assert_eq!(
        backend.token.lock().unwrap().as_deref(),
        Some("original-token"),
        "an uncertain activation must restore the exact snapshot"
    );
    assert_eq!(
        load_token_from_profile(profile.path()).as_deref(),
        Some("target-token"),
        "restoration uncertainty must never persist the original system credential into the target profile"
    );
}

#[derive(Clone)]
struct RestoreUncertainBackend {
    token: Arc<Mutex<Option<String>>>,
    timed_writes: Arc<Mutex<usize>>,
}

impl KeychainBackend for RestoreUncertainBackend {
    fn read_token(&self) -> Result<Option<String>, String> {
        Ok(self.token.lock().unwrap().clone())
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        *self.token.lock().unwrap() = Some(token.to_string());
        Ok(())
    }

    fn delete_token(&self) -> Result<(), String> {
        *self.token.lock().unwrap() = None;
        Ok(())
    }

    fn write_token_until(&self, token: &str, _deadline: Instant) -> Result<(), String> {
        let mut writes = self.timed_writes.lock().unwrap();
        *writes += 1;
        *self.token.lock().unwrap() = Some(token.to_string());
        if *writes == 1 {
            Ok(())
        } else {
            Err("restore result uncertain after mutation".to_string())
        }
    }
}

#[test]
fn test_uncertain_restore_never_cross_contaminates_profile_on_drop() {
    let profile = tempdir().unwrap();
    save_token_to_profile(profile.path(), "target-token").unwrap();
    let backend = RestoreUncertainBackend {
        token: Arc::new(Mutex::new(Some("original-token".to_string()))),
        timed_writes: Arc::new(Mutex::new(0)),
    };

    let guard = KeychainGuard::new_with_backend(profile.path(), backend.clone()).unwrap();
    *backend.token.lock().unwrap() = Some("refreshed-target-token".to_string());
    let result = guard.sync_and_restore_until(Instant::now() + Duration::from_secs(1));

    assert!(result.is_err());
    assert_eq!(
        backend.token.lock().unwrap().as_deref(),
        Some("original-token")
    );
    assert_eq!(
        load_token_from_profile(profile.path()).as_deref(),
        Some("refreshed-target-token"),
        "Drop must retry restoration only; it must not persist the restored system token"
    );
}

#[derive(Clone)]
struct PersistenceConsumesDeadlineBackend {
    token: Arc<Mutex<Option<String>>>,
    writes: Arc<Mutex<Vec<String>>>,
    timed_reads: Arc<Mutex<usize>>,
}

impl KeychainBackend for PersistenceConsumesDeadlineBackend {
    fn read_token(&self) -> Result<Option<String>, String> {
        Ok(self.token.lock().unwrap().clone())
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        self.writes.lock().unwrap().push(token.to_string());
        *self.token.lock().unwrap() = Some(token.to_string());
        Ok(())
    }

    fn delete_token(&self) -> Result<(), String> {
        *self.token.lock().unwrap() = None;
        Ok(())
    }

    fn read_token_until(&self, deadline: Instant) -> Result<Option<String>, String> {
        let mut timed_reads = self.timed_reads.lock().unwrap();
        *timed_reads += 1;
        if *timed_reads == 1 {
            return Ok(self.token.lock().unwrap().clone());
        }
        drop(timed_reads);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        Err("persistence read exhausted deadline".to_string())
    }
}

#[test]
fn test_keychain_cleanup_restores_after_persistence_deadline() {
    let profile = tempdir().unwrap();
    save_token_to_profile(profile.path(), "target-token").unwrap();
    let backend = PersistenceConsumesDeadlineBackend {
        token: Arc::new(Mutex::new(Some("original-token".to_string()))),
        writes: Arc::new(Mutex::new(Vec::new())),
        timed_reads: Arc::new(Mutex::new(0)),
    };

    let guard = KeychainGuard::new_with_backend(profile.path(), backend.clone()).unwrap();
    *backend.token.lock().unwrap() = Some("refreshed-target-token".to_string());

    let result = guard.sync_and_restore_until(Instant::now() + Duration::from_millis(5));
    assert!(result.is_err());
    assert_eq!(
        backend.token.lock().unwrap().as_deref(),
        Some("original-token"),
        "restoration must still run when persistence consumes its deadline"
    );
}

#[test]
fn test_save_and_load_profile_token() {
    let temp = tempdir().unwrap();
    let profile_dir = temp.path();

    let payload_json = json!({
        "email": "carol@gmail.com"
    })
    .to_string();
    let jwt = format!("header.{}.sig", URL_SAFE_NO_PAD.encode(&payload_json));
    let token_str = json!({ "id_token": jwt }).to_string();

    let email = save_token_to_profile(profile_dir, &token_str).unwrap();
    assert_eq!(email, Some("carol@gmail.com".to_string()));

    let loaded = load_token_from_profile(profile_dir);
    assert_eq!(loaded, Some(token_str));
}
