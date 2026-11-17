//! Verify the CRX3 developer signature before decoding any archive entries.
use base64::Engine;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};
pub const MAX_CRX: u64 = 64 * 1024 * 1024;
pub struct Verified<'a> {
    pub key: String,
    pub archive: &'a [u8],
}
pub fn verify<'a>(bytes: &'a [u8], id: &str) -> Result<Verified<'a>, String> {
    let bad =
        || "Подпись CRX3 отсутствует, повреждена или не соответствует ID расширения".to_string();
    if !crate::security::extension_id(id)
        || bytes.len() as u64 > MAX_CRX
        || bytes.get(..8) != Some(b"Cr24\x03\0\0\0")
    {
        return Err(bad());
    }
    let length = u32::from_le_bytes(bytes.get(8..12).ok_or_else(bad)?.try_into().unwrap()) as usize;
    if length > 1024 * 1024 {
        return Err(bad());
    }
    let end = 12usize.checked_add(length).ok_or_else(bad)?;
    let header =
        crate::ext::protobuf_fields(bytes.get(12..end).ok_or_else(bad)?).ok_or_else(bad)?;
    let signed: Vec<_> = header.iter().filter(|(n, _)| *n == 10000).collect();
    if signed.len() != 1 {
        return Err(bad());
    }
    let signed = signed[0].1;
    let fields = crate::ext::protobuf_fields(signed).ok_or_else(bad)?;
    let ids: Vec<_> = fields.iter().filter(|(n, _)| *n == 1).collect();
    if ids.len() != 1 || ids[0].1.len() != 16 {
        return Err(bad());
    }
    let declared: String = ids[0]
        .1
        .iter()
        .flat_map(|b| [(b'a' + (b >> 4)) as char, (b'a' + (b & 15)) as char])
        .collect();
    if declared != id {
        return Err(bad());
    }
    let archive = bytes
        .get(end..)
        .filter(|b| b.starts_with(b"PK\x03\x04"))
        .ok_or_else(bad)?;
    let mut message = b"CRX3 SignedData\0".to_vec();
    message.extend_from_slice(&(signed.len() as u32).to_le_bytes());
    message.extend_from_slice(signed);
    message.extend_from_slice(archive);
    for (kind, proof) in header.iter().filter(|(n, _)| *n == 2 || *n == 3) {
        let Some(fields) = crate::ext::protobuf_fields(proof) else {
            continue;
        };
        let keys: Vec<_> = fields.iter().filter(|(n, _)| *n == 1).collect();
        let signatures: Vec<_> = fields.iter().filter(|(n, _)| *n == 2).collect();
        if keys.len() != 1 || signatures.len() != 1 {
            continue;
        }
        let key = keys[0].1;
        let hash = Sha256::digest(key);
        if hash[..16] != ids[0].1[..] {
            continue;
        }
        let Ok(spki) = spki::SubjectPublicKeyInfoRef::try_from(key) else {
            continue;
        };
        let Some(public) = spki.subject_public_key.as_bytes() else {
            continue;
        };
        let algorithm: &dyn ring::signature::VerificationAlgorithm =
            if *kind == 2 && spki.algorithm.oid.to_string() == "1.2.840.113549.1.1.1" {
                &ring::signature::RSA_PKCS1_2048_8192_SHA256
            } else if *kind == 3 && spki.algorithm.oid.to_string() == "1.2.840.10045.2.1" {
                &ring::signature::ECDSA_P256_SHA256_ASN1
            } else {
                continue;
            };
        if ring::signature::UnparsedPublicKey::new(algorithm, public)
            .verify(&message, signatures[0].1)
            .is_ok()
        {
            return Ok(Verified {
                key: base64::engine::general_purpose::STANDARD.encode(key),
                archive,
            });
        }
    }
    Err(bad())
}
fn entry_path(name: &str) -> Result<PathBuf, String> {
    let normalized = name.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') || normalized.len() > 1024 {
        return Err("Недопустимый путь ZIP".into());
    }
    let mut result = PathBuf::new();
    for part in normalized.trim_end_matches('/').split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part
                .chars()
                .any(|c| c.is_control() || "<>:\"|?*".contains(c))
            || part.ends_with(['.', ' '])
            || crate::downloads::filename(part) != part
        {
            return Err("Небезопасное имя файла ZIP".into());
        }
        result.push(part);
    }
    Ok(result)
}
pub fn extract(bytes: &[u8], dest: &Path) -> Result<(), String> {
    // Bound metadata allocation before constructing ZipArchive. Extensions at
    // these size limits need neither ZIP64, split disks nor trailing data.
    let eocd = bytes
        .windows(4)
        .rposition(|v| v == b"PK\x05\x06")
        .ok_or("Нет конца ZIP")?;
    let end = bytes
        .get(eocd..)
        .filter(|v| v.len() >= 22)
        .ok_or("Обрезанный ZIP")?;
    let u16at = |n| u16::from_le_bytes([end[n], end[n + 1]]) as usize;
    let u32at = |n| u32::from_le_bytes(end[n..n + 4].try_into().unwrap()) as usize;
    if end.len() != 22 + u16at(20)
        || u16at(4) != 0
        || u16at(6) != 0
        || u16at(8) != u16at(10)
        || u16at(10) > 10000
        || u32at(12).checked_add(u32at(16)) != Some(eocd)
        || eocd >= 20 && bytes.get(eocd - 20..eocd - 16) == Some(b"PK\x06\x07")
    {
        return Err("Недопустимая структура или размер ZIP".into());
    }
    let mut archive = zip::ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        Cursor::new(bytes),
    )
    .map_err(|e| e.to_string())?;
    if archive.len() > 10000 {
        return Err("Слишком много файлов в расширении".into());
    }
    let mut total = 0u64;
    let mut seen = HashSet::new();
    archive
        .validate_header_consistency()
        .map_err(|e| e.to_string())?;
    if archive.has_duplicate_names() {
        return Err("Повторяющиеся имена ZIP".into());
    }
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let relative = entry_path(&file.name().map_err(|e| e.to_string())?)?;
        if !seen.insert(relative.to_string_lossy().to_lowercase()) {
            return Err("Повторяющиеся имена в расширении".into());
        }
        let kind = file.unix_mode().unwrap_or(0) & 0o170000;
        if !matches!(kind, 0 | 0o100000 | 0o040000) {
            return Err("Ссылки и специальные файлы в расширении запрещены".into());
        }
        total = total
            .checked_add(file.size())
            .ok_or("Слишком большой архив")?;
        if total > 256 * 1024 * 1024 || file.size() > 64 * 1024 * 1024 {
            return Err("Расширение превышает лимит распаковки".into());
        }
        let path = dest.join(relative);
        if file.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            continue;
        }
        std::fs::create_dir_all(path.parent().ok_or("Пустой путь")?).map_err(|e| e.to_string())?;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let expected = file.size();
        let actual = std::io::copy(
            &mut file.by_ref().take(expected.saturating_add(1)),
            &mut out,
        )
        .map_err(|e| e.to_string())?;
        if actual != expected {
            return Err("Размер файла ZIP не соответствует заголовку".into());
        }
        out.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_archive_escape_and_devices() {
        for p in [
            "../evil",
            "a/../../evil",
            "/evil",
            "C:\\evil",
            "\\\\host\\x",
            "a:stream",
            "CON.txt",
            "dir/NUL",
            "a./b",
            "a/../b",
        ] {
            assert!(entry_path(p).is_err(), "{p}");
        }
        assert_eq!(
            entry_path("folder/script.js").unwrap(),
            Path::new("folder/script.js")
        );
    }
    #[test]
    fn zip_limits_reject_duplicates_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["A.txt", "a.txt"] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"data").unwrap();
        }
        assert!(extract(&writer.finish().unwrap().into_inner(), dir.path()).is_err());
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink(
                "link",
                "../escape",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        assert!(extract(&writer.finish().unwrap().into_inner(), dir.path()).is_err());
    }
    #[test]
    fn malformed_headers_do_not_panic() {
        let id = "mmioliijnhnoblpgimnlajmefafdfilb";
        let mut seed = 0x12345678u32;
        for n in 0..10000 {
            let mut data = vec![0u8; n % 513];
            for b in &mut data {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *b = seed as u8;
            }
            if data.len() > 12 && n % 2 == 0 {
                let len = (data.len() - 12) as u32;
                data[..8].copy_from_slice(b"Cr24\x03\0\0\0");
                data[8..12].copy_from_slice(&len.to_le_bytes());
            }
            let _ = verify(&data, id);
            let _ = crate::ext::protobuf_fields(&data);
        }
    }
    #[test]
    fn zip_traversal_and_metadata_bombs_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("../outside.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"blocked").unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        assert!(extract(&bytes, dir.path()).is_err());
        assert!(!dir.path().parent().unwrap().join("outside.txt").exists());
        let end = bytes.windows(4).rposition(|v| v == b"PK\x05\x06").unwrap();
        bytes[end + 8..end + 12].fill(0xff);
        assert!(extract(&bytes, dir.path()).is_err());
    }
    #[test]
    #[ignore = "Requires official CRX fixtures in LUMEN_CRX_FIXTURES"]
    fn official_signatures_and_tampering() {
        let root = PathBuf::from(std::env::var_os("LUMEN_CRX_FIXTURES").unwrap());
        for (name, id) in [
            ("shazam.crx", "mmioliijnhnoblpgimnlajmefafdfilb"),
            ("vknext.crx", "jephanpkonkmnkekmlkcijdjgniikppl"),
        ] {
            let mut bytes = std::fs::read(root.join(name)).unwrap();
            let v = verify(&bytes, id).unwrap();
            let dir = tempfile::tempdir().unwrap();
            extract(v.archive, dir.path()).unwrap();
            assert!(dir.path().join("manifest.json").is_file());
            let n = bytes.len();
            bytes[n - 1] ^= 1;
            assert!(verify(&bytes, id).is_err(), "modified archive accepted");
            bytes[n - 1] ^= 1;
            let header_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
            let fields = crate::ext::protobuf_fields(&bytes[12..12 + header_len]).unwrap();
            let signed = fields.iter().find(|(n, _)| *n == 10000).unwrap().1;
            let offset = signed.as_ptr() as usize - bytes.as_ptr() as usize + signed.len() - 1;
            bytes[offset] ^= 1;
            assert!(
                verify(&bytes, id).is_err(),
                "modified signed header accepted"
            );
        }
    }
}
