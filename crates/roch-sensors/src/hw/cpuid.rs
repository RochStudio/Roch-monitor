//! The processor's own identity, from CPUID.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpuId {
    pub vendor: String,
    pub brand: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    /// Leaf 7 EDX bit 15: the part mixes performance and efficiency cores.
    pub hybrid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Amd,
    Other,
}

impl CpuId {
    #[cfg(target_arch = "x86_64")]
    pub fn read() -> CpuId {
        use std::arch::x86_64::__cpuid;
        #[allow(unused_unsafe)]
        // SAFETY: CPUID exists on every x86-64 processor.
        let leaf = |l: u32| unsafe { __cpuid(l) };
        let l0 = leaf(0);
        let mut vendor = Vec::with_capacity(12);
        for r in [l0.ebx, l0.edx, l0.ecx] {
            vendor.extend_from_slice(&r.to_le_bytes());
        }
        let l1 = leaf(1);
        let base_family = (l1.eax >> 8) & 0xF;
        let base_model = (l1.eax >> 4) & 0xF;
        let ext_family = (l1.eax >> 20) & 0xFF;
        let ext_model = (l1.eax >> 16) & 0xF;
        let family = if base_family == 0xF { base_family + ext_family } else { base_family };
        let model = if base_family == 0x6 || base_family == 0xF { (ext_model << 4) | base_model } else { base_model };
        let hybrid = l0.eax >= 7 && (leaf(7).edx >> 15) & 1 == 1;
        let mut brand = Vec::with_capacity(48);
        if leaf(0x8000_0000).eax >= 0x8000_0004 {
            for l in 0x8000_0002..=0x8000_0004u32 {
                let r = leaf(l);
                for v in [r.eax, r.ebx, r.ecx, r.edx] {
                    brand.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        let brand = String::from_utf8_lossy(&brand).trim_matches(char::from(0)).trim().to_owned();
        CpuId {
            vendor: String::from_utf8_lossy(&vendor).into_owned(),
            brand: clean_brand(&brand),
            family,
            model,
            stepping: l1.eax & 0xF,
            hybrid,
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn read() -> CpuId {
        CpuId::default()
    }

    pub fn vendor(&self) -> Vendor {
        match self.vendor.as_str() {
            "GenuineIntel" => Vendor::Intel,
            "AuthenticAMD" => Vendor::Amd,
            _ => Vendor::Other,
        }
    }

    /// Alder Lake, Raptor Lake and Raptor Lake Refresh desktop: the LGA 1700 parts whose MCHBAR
    /// layout Roch Viewer reads.
    pub fn is_lga1700(&self) -> bool {
        self.vendor() == Vendor::Intel && self.family == 6 && matches!(self.model, 0x97 | 0x9A | 0xB7 | 0xBA | 0xBF)
    }

    /// Core Ultra 200S (Arrow Lake-S), whose memory-controller block moved by 0x10000.
    pub fn is_arrow_lake(&self) -> bool {
        self.vendor() == Vendor::Intel
            && self.family == 6
            && (matches!(self.model, 0xC5 | 0xC6) || self.brand.to_uppercase().contains("CORE(TM) ULTRA"))
    }
}

/// "Intel(R) Core(TM) i9-14900KF" -> "Intel Core i9-14900KF"; padding and trademarks out.
pub fn clean_brand(brand: &str) -> String {
    let mut s = brand.replace("(R)", "").replace("(TM)", "").replace("(tm)", "");
    for tail in [" CPU @", " @ "] {
        if let Some(at) = s.find(tail) {
            s.truncate(at);
        }
    }
    for suffix in [" Processor", " processor"] {
        if let Some(at) = s.find(suffix) {
            // "AMD Ryzen 9 9950X 16-Core Processor" -> drop the core count with it.
            let head = &s[..at];
            let head = match head.rfind(' ') {
                Some(i) if head[i + 1..].ends_with("-Core") => &head[..i],
                _ => head,
            };
            s = head.to_owned();
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brands_lose_their_trademarks() {
        assert_eq!(clean_brand("Intel(R) Core(TM) i9-14900KF"), "Intel Core i9-14900KF");
        assert_eq!(clean_brand("AMD Ryzen 9 9950X3D 16-Core Processor           "), "AMD Ryzen 9 9950X3D");
        assert_eq!(clean_brand("Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz"), "Intel Core i7-8700K");
    }

    #[test]
    fn raptor_lake_refresh_is_lga1700() {
        let c = CpuId { vendor: "GenuineIntel".into(), family: 6, model: 0xB7, ..Default::default() };
        assert!(c.is_lga1700());
        assert!(!c.is_arrow_lake());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn this_machine_answers() {
        let c = CpuId::read();
        assert!(!c.vendor.is_empty());
        assert!(c.family > 0);
    }
}
