use crate::config::FontAttributes;
use anyhow::{anyhow, Error};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Mutex;

#[cfg(all(unix, not(target_os = "macos")))]
pub mod font_config;
#[cfg(any(target_os = "macos", windows))]
pub mod font_kit;
#[cfg(any(target_os = "macos", windows))]
pub mod font_loader;

/// Represents the data behind a font.
/// This may be a font file that we can read off disk,
/// or some data that resides in memory.
/// The `index` parameter is the index into a font
/// collection if the data represents a collection of
/// fonts.
#[derive(Clone)]
pub enum FontDataHandle {
    OnDisk {
        path: PathBuf,
        index: u32,
    },
    #[allow(dead_code)]
    Memory {
        data: Vec<u8>,
        index: u32,
    },
}

impl std::fmt::Debug for FontDataHandle {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        match self {
            Self::OnDisk { path, index } => fmt
                .debug_struct("OnDisk")
                .field("path", &path)
                .field("index", &index)
                .finish(),
            Self::Memory { data, index } => fmt
                .debug_struct("Memory")
                .field("data_len", &data.len())
                .field("index", &index)
                .finish(),
        }
    }
}

pub trait FontLocator {
    /// Given a font selection, return the list of successfully loadable
    /// FontDataHandle's that correspond to it
    fn load_fonts(&self, fonts_selection: &[FontAttributes])
        -> anyhow::Result<Vec<FontDataHandle>>;
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum FontLocatorSelection {
    /// Use fontconfig APIs to resolve fonts (!macos, posix systems)
    FontConfig,
    /// Use the fontloader crate to use a system specific method of
    /// resolving fonts
    FontLoader,
    /// Use the fontkit crate to use a different system specific
    /// method of resolving fonts
    FontKit,
    /// Use only the font_dirs configuration to locate fonts
    ConfigDirsOnly,
}

lazy_static::lazy_static! {
    static ref DEFAULT_LOCATOR: Mutex<FontLocatorSelection> = Mutex::new(Default::default());
}

impl Default for FontLocatorSelection {
    fn default() -> Self {
        if cfg!(all(unix, not(target_os = "macos"))) {
            FontLocatorSelection::FontConfig
        } else {
            FontLocatorSelection::FontLoader
        }
    }
}

impl FontLocatorSelection {
    pub fn set_default(self) {
        let mut def = DEFAULT_LOCATOR.lock().unwrap();
        *def = self;
    }

    pub fn get_default() -> Self {
        let def = DEFAULT_LOCATOR.lock().unwrap();
        *def
    }

    pub fn variants() -> Vec<&'static str> {
        vec!["FontConfig", "FontLoader", "FontKit", "ConfigDirsOnly"]
    }

    pub fn new_locator(self) -> Box<dyn FontLocator> {
        match self {
            Self::FontConfig => {
                #[cfg(all(unix, not(target_os = "macos")))]
                return Box::new(font_config::FontConfigFontLocator {});
                #[cfg(not(all(unix, not(target_os = "macos"))))]
                panic!("fontconfig not compiled in");
            }
            Self::FontLoader => {
                #[cfg(any(target_os = "macos", windows))]
                return Box::new(font_loader::FontLoaderFontLocator {});
                #[cfg(not(any(target_os = "macos", windows)))]
                panic!("fontloader not compiled in");
            }
            Self::FontKit => {
                #[cfg(any(target_os = "macos", windows))]
                return Box::new(::font_kit::source::SystemSource::new());
                #[cfg(not(any(target_os = "macos", windows)))]
                panic!("fontkit not compiled in");
            }
            Self::ConfigDirsOnly => Box::new(NopSystemSource {}),
        }
    }
}

impl std::str::FromStr for FontLocatorSelection {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_ref() {
            "fontconfig" => Ok(Self::FontConfig),
            "fontloader" => Ok(Self::FontLoader),
            "fontkit" => Ok(Self::FontKit),
            "configdirsonly" => Ok(Self::ConfigDirsOnly),
            _ => Err(anyhow!(
                "{} is not a valid FontLocatorSelection variant, possible values are {:?}",
                s,
                Self::variants()
            )),
        }
    }
}

struct NopSystemSource {}

impl FontLocator for NopSystemSource {
    fn load_fonts(
        &self,
        _fonts_selection: &[FontAttributes],
    ) -> anyhow::Result<Vec<FontDataHandle>> {
        Ok(vec![])
    }
}
