//! Binaire universel (Intel et ARM réunis) réduit à l'architecture du Mac à l'installation. Le
//! format « fat » de Mach-O est un en-tête suivi de programmes complets, chacun signé à part :
//! recopier le bon suffit. Sans `lipo`, qui n'existe qu'avec les outils de développement.

use std::path::Path;

use anyhow::Result;

use super::output;

/// En-tête universel ; l'en-tête et ses entrées sont en ordre réseau.
const FAT_MAGIC: u32 = 0xcafe_babe;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
/// Une entrée : type et sous-type de processeur, position, taille, alignement.
const ENTRY: usize = 20;

/// Le programme du processeur `cpu` dans un binaire universel ; `None` si le fichier n'en est
/// pas un ou ne le contient pas.
pub(super) fn slice(data: &[u8], cpu: u32) -> Option<&[u8]> {
    let word = |at: usize| Some(u32::from_be_bytes(data.get(at..at.checked_add(4)?)?.try_into().ok()?));
    if word(0)? != FAT_MAGIC {
        return None;
    }
    // Un binaire universel n'a que quelques architectures ; au-delà, ce n'en est pas un.
    let count = usize::try_from(word(4)?).ok()?.min(16);
    (0..count).find_map(|i| {
        let entry = 8 + i * ENTRY;
        (word(entry)? == cpu).then_some(())?;
        let offset = usize::try_from(word(entry + 8)?).ok()?;
        let size = usize::try_from(word(entry + 12)?).ok()?;
        data.get(offset..offset.checked_add(size)?)
    })
}

/// Processeur de la machine, même si `abdns` tourne sous Rosetta sur un Mac ARM.
fn machine() -> u32 {
    let translated = output("/usr/sbin/sysctl", &["-n", "sysctl.proc_translated"]).is_some_and(|v| v.trim() == "1");
    if cfg!(target_arch = "aarch64") || translated { CPU_TYPE_ARM64 } else { CPU_TYPE_X86_64 }
}

/// Réduit `path` sur place s'il est universel : copie écrite à côté puis renommée (un programme
/// en cours garde l'ancienne). Rend la taille avant et après.
pub(super) fn thin(path: &Path) -> Result<Option<(usize, usize)>> {
    let data = std::fs::read(path)?;
    let Some(program) = slice(&data, machine()) else { return Ok(None) };
    let copy = path.with_extension("thin");
    std::fs::write(&copy, program)?;
    std::fs::set_permissions(&copy, std::fs::metadata(path)?.permissions())?;
    std::fs::rename(&copy, path)?;
    Ok(Some((data.len(), program.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// En-tête universel de deux programmes, `x86` puis `arm`, alignés comme le fait `lipo`.
    fn universal(x86: &[u8], arm: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&FAT_MAGIC.to_be_bytes());
        data.extend_from_slice(&2u32.to_be_bytes());
        let (first, second) = (64u32, 64 + u32::try_from(x86.len()).unwrap());
        for (cpu, offset, size) in [(CPU_TYPE_X86_64, first, x86.len()), (CPU_TYPE_ARM64, second, arm.len())] {
            for word in [cpu, 3, offset, u32::try_from(size).unwrap(), 6] {
                data.extend_from_slice(&word.to_be_bytes());
            }
        }
        data.resize(64, 0);
        data.extend_from_slice(x86);
        data.extend_from_slice(arm);
        data
    }

    #[test]
    fn slices_of_a_universal_binary() {
        let data = universal(b"intel program", b"arm program");
        assert_eq!(slice(&data, CPU_TYPE_ARM64), Some(&b"arm program"[..]));
        assert_eq!(slice(&data, CPU_TYPE_X86_64), Some(&b"intel program"[..]));
        assert_eq!(slice(&data, 0x0100_0012), None);
        // Programme seul (Mach-O 64 bits) ou en-tête tronqué : rien à réduire.
        assert_eq!(slice(&[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0], CPU_TYPE_ARM64), None);
        assert_eq!(slice(&data[..30], CPU_TYPE_ARM64), None);
        // Entrée qui sort du fichier : refusée.
        let mut broken = data.clone();
        broken[8 + ENTRY + 12..8 + ENTRY + 16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(slice(&broken, CPU_TYPE_ARM64), None);
    }

    /// Sur un vrai binaire de `lipo` : `ABDNS_UNIVERSAL=chemin cargo test -- --ignored universal`.
    #[test]
    #[ignore = "binaire universel construit à part"]
    fn universal_binary_from_lipo() {
        let source = std::env::var("ABDNS_UNIVERSAL").expect("ABDNS_UNIVERSAL");
        let copy = std::env::temp_dir().join("abdns-thin-test");
        std::fs::copy(&source, &copy).unwrap();
        let (before, after) = thin(&copy).unwrap().expect("universel");
        assert!(after < before);
        assert_eq!(thin(&copy).unwrap(), None, "déjà réduit");
        let version = std::process::Command::new(&copy).arg("--version").output().unwrap();
        assert!(version.status.success());
        let signed = std::process::Command::new("/usr/bin/codesign").arg("-v").arg(&copy).status().unwrap();
        assert!(signed.success(), "toujours signé");
        std::fs::remove_file(&copy).ok();
    }
}
