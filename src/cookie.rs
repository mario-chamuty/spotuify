//! Automatic discovery of the Spotify `sp_dc` cookie from local browser
//! profiles, so the real Home works with zero manual setup.
//!
//! Firefox keeps cookies in a plaintext SQLite DB (`cookies.sqlite`); we read
//! `sp_dc` for `.spotify.com` directly. Chromium-family browsers encrypt cookie
//! values; on Linux we decrypt them with the standard scheme (AES-128-CBC with a
//! PBKDF2-derived key). Everything is read-only and used solely to authenticate
//! to Spotify; if nothing is found we return `None` and the caller falls back.

use std::path::{Path, PathBuf};

use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use rusqlite::{Connection, OpenFlags};
use sha1::Sha1;

type Aes128CbcDec = cbc::Decryptor<Aes128>;

enum Browser {
    Firefox,
    Chromium,
}

/// The effective cookie: an explicit config value wins; otherwise auto-detect.
pub fn resolve(config_value: &str) -> String {
    if !config_value.trim().is_empty() {
        return config_value.trim().to_string();
    }
    detect_sp_dc().unwrap_or_default()
}

/// Find an `sp_dc` cookie from any local browser, newest profile first.
pub fn detect_sp_dc() -> Option<String> {
    let mut candidates: Vec<(std::time::SystemTime, PathBuf, Browser)> = Vec::new();
    for db in firefox_cookie_dbs() {
        if let Some(t) = modified(&db) {
            candidates.push((t, db, Browser::Firefox));
        }
    }
    for db in chromium_cookie_dbs() {
        if let Some(t) = modified(&db) {
            candidates.push((t, db, Browser::Chromium));
        }
    }
    // Newest-used profile first — most likely to hold a live session.
    candidates.sort_by_key(|(t, _, _)| std::cmp::Reverse(*t));

    for (_, db, browser) in candidates {
        let found = match browser {
            Browser::Firefox => firefox_sp_dc(&db),
            Browser::Chromium => chromium_sp_dc(&db),
        };
        if let Some(v) = found {
            if !v.is_empty() {
                tracing::info!("auto-detected sp_dc cookie from {}", db.display());
                return Some(v);
            }
        }
    }
    None
}

fn modified(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

/// Open a cookie DB read-only and immutable, so a running browser's WAL lock
/// doesn't block us.
fn open_ro(path: &Path) -> Option<Connection> {
    let uri = format!(
        "file:{}?immutable=1",
        path.to_string_lossy().replace(' ', "%20")
    );
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()
}

// ---- Firefox -------------------------------------------------------------

fn firefox_cookie_dbs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let roots = [
        home.join(".mozilla/firefox"),
        home.join("snap/firefox/common/.mozilla/firefox"),
        home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
    ];
    let mut dbs = Vec::new();
    for root in roots {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for e in entries.flatten() {
                let p = e.path().join("cookies.sqlite");
                if p.is_file() {
                    dbs.push(p);
                }
            }
        }
    }
    dbs
}

fn firefox_sp_dc(db: &Path) -> Option<String> {
    let conn = open_ro(db)?;
    conn.query_row(
        "SELECT value FROM moz_cookies \
         WHERE host LIKE '%spotify.com' AND name = 'sp_dc' \
         ORDER BY length(value) DESC LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

// ---- Chromium family (Linux) ---------------------------------------------

fn chromium_cookie_dbs() -> Vec<PathBuf> {
    let mut dbs = Vec::new();
    let bases = [
        "google-chrome",
        "google-chrome-beta",
        "chromium",
        "BraveSoftware/Brave-Browser",
        "microsoft-edge",
        "vivaldi",
    ];
    let profiles = ["Default", "Profile 1", "Profile 2"];
    if let Some(cfg) = dirs::config_dir() {
        for b in bases {
            for pr in profiles {
                // Newer Chromium keeps cookies under Network/, older at the root.
                for sub in [PathBuf::from("Cookies"), PathBuf::from("Network/Cookies")] {
                    let p = cfg.join(b).join(pr).join(&sub);
                    if p.is_file() {
                        dbs.push(p);
                    }
                }
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        for p in [
            home.join("snap/chromium/common/chromium/Default/Cookies"),
            home.join("snap/chromium/common/chromium/Default/Network/Cookies"),
        ] {
            if p.is_file() {
                dbs.push(p);
            }
        }
    }
    dbs
}

fn chromium_sp_dc(db: &Path) -> Option<String> {
    let conn = open_ro(db)?;
    let enc: Vec<u8> = conn
        .query_row(
            "SELECT encrypted_value FROM cookies \
             WHERE host_key LIKE '%spotify.com' AND name = 'sp_dc' \
             ORDER BY length(encrypted_value) DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok()?;
    decrypt_chromium_linux(&enc)
}

/// Decrypt a Chromium cookie value on Linux. Values are `v10`/`v11` prefixed and
/// AES-128-CBC encrypted with a PBKDF2(SHA1) key. The password is the OS
/// keyring's "Safe Storage" secret; without a keyring Chromium uses the literal
/// `peanuts`, which is what we try (covers headless / no-keyring setups). If the
/// keyring is in use we can't read it here and simply fail over to Firefox.
fn decrypt_chromium_linux(enc: &[u8]) -> Option<String> {
    if enc.len() < 3 {
        return None;
    }
    let prefix = &enc[..3];
    if prefix != b"v10" && prefix != b"v11" {
        // Unencrypted (very old Chromium).
        return String::from_utf8(enc.to_vec()).ok();
    }
    let body = &enc[3..];
    if body.len() < 16 || !body.len().is_multiple_of(16) {
        return None;
    }

    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<Sha1>(b"peanuts", b"saltysalt", 1, &mut key);
    let iv = [0x20u8; 16];

    let cipher = Aes128CbcDec::new_from_slices(&key, &iv).ok()?;
    let mut buf = body.to_vec();
    let pt = cipher.decrypt_padded_mut::<Pkcs7>(&mut buf).ok()?;

    if let Ok(s) = std::str::from_utf8(pt) {
        return Some(s.to_string());
    }
    // Some builds prepend a 32-byte SHA-256 domain hash to the plaintext.
    if pt.len() > 32 {
        return std::str::from_utf8(&pt[32..]).ok().map(str::to_string);
    }
    None
}
