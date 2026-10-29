//! User-editable file categories: each groups file extensions and may have
//! its own save folder. Files that match no category belong to "Other".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::naming::sanitize_file_name;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryId(pub u32);

impl CategoryId {
    /// The built-in catch-all; it cannot be removed.
    pub const OTHER: CategoryId = CategoryId(0);
}

/// Which icon and colors a category is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryLook {
    Archive,
    Document,
    Music,
    Program,
    Video,
    Image,
    Generic,
}

impl CategoryLook {
    pub const ALL: [CategoryLook; 7] = [
        CategoryLook::Generic,
        CategoryLook::Archive,
        CategoryLook::Document,
        CategoryLook::Music,
        CategoryLook::Program,
        CategoryLook::Video,
        CategoryLook::Image,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CategoryLook::Archive => "Archive",
            CategoryLook::Document => "Document",
            CategoryLook::Music => "Music",
            CategoryLook::Program => "Program",
            CategoryLook::Video => "Video",
            CategoryLook::Image => "Image",
            CategoryLook::Generic => "Generic",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: CategoryId,
    pub name: String,
    /// Lowercase, without the dot.
    pub extensions: Vec<String>,
    /// Save folder; `None` uses `<download folder>/<name>`.
    pub folder: Option<PathBuf>,
    pub look: CategoryLook,
}

impl Category {
    /// The folder files of this category are saved in.
    pub fn save_folder(&self, download_dir: &Path) -> PathBuf {
        match &self.folder {
            Some(folder) => folder.clone(),
            None => download_dir.join(sanitize_file_name(&self.name)),
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CategoryError {
    #[error("a category needs a name")]
    EmptyName,
    #[error("a category named {0:?} already exists")]
    DuplicateName(String),
    #[error("the Other category cannot be removed")]
    OtherIsPermanent,
    #[error("category {0:?} does not exist")]
    Unknown(CategoryId),
}

/// Every category, with "Other" always present.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<Category>", into = "Vec<Category>")]
pub struct Categories {
    list: Vec<Category>,
}

impl Default for Categories {
    fn default() -> Self {
        let defined = |id, name: &str, look, extensions: &str| Category {
            id: CategoryId(id),
            name: name.to_owned(),
            extensions: parse_extensions(extensions),
            folder: None,
            look,
        };
        Self {
            list: vec![
                defined(
                    1,
                    "Compressed",
                    CategoryLook::Archive,
                    "zip rar 7z tar gz tgz bz2 xz zst iso cab",
                ),
                defined(
                    2,
                    "Documents",
                    CategoryLook::Document,
                    "pdf doc docx xls xlsx ppt pptx odt ods txt rtf epub csv md",
                ),
                defined(
                    3,
                    "Music",
                    CategoryLook::Music,
                    "mp3 flac wav aac ogg m4a opus wma",
                ),
                defined(
                    4,
                    "Programs",
                    CategoryLook::Program,
                    "exe msi msix dmg pkg deb rpm appimage apk",
                ),
                defined(
                    5,
                    "Video",
                    CategoryLook::Video,
                    "mp4 mkv webm avi mov wmv flv m4v mpg mpeg 3gp ts",
                ),
                other(),
            ],
        }
    }
}

fn other() -> Category {
    Category {
        id: CategoryId::OTHER,
        name: "Other".to_owned(),
        extensions: Vec::new(),
        folder: None,
        look: CategoryLook::Generic,
    }
}

/// Splits `"zip, .RAR 7z"` into `["zip", "rar", "7z"]`, without duplicates.
pub fn parse_extensions(text: &str) -> Vec<String> {
    let mut extensions: Vec<String> = Vec::new();
    for raw in text.split([' ', ',', ';', '\t', '\n']) {
        let extension = raw.trim().trim_start_matches('.').to_ascii_lowercase();
        if !extension.is_empty() && !extensions.contains(&extension) {
            extensions.push(extension);
        }
    }
    extensions
}

impl From<Vec<Category>> for Categories {
    /// Accepts a saved list, restoring "Other" if an edit removed it.
    fn from(mut list: Vec<Category>) -> Self {
        if !list.iter().any(|category| category.id == CategoryId::OTHER) {
            list.push(other());
        }
        Self { list }
    }
}

impl From<Categories> for Vec<Category> {
    fn from(categories: Categories) -> Self {
        categories.list
    }
}

impl Categories {
    pub fn all(&self) -> &[Category] {
        &self.list
    }

    /// The category with `id`, or Other when it was removed meanwhile.
    pub fn get(&self, id: CategoryId) -> &Category {
        self.list
            .iter()
            .find(|category| category.id == id)
            .or_else(|| {
                self.list
                    .iter()
                    .find(|category| category.id == CategoryId::OTHER)
            })
            .expect("Other is always present")
    }

    /// The first category listing the file's extension, else Other.
    pub fn classify(&self, file_name: &str) -> CategoryId {
        let Some((_, extension)) = file_name.rsplit_once('.') else {
            return CategoryId::OTHER;
        };
        let extension = extension.to_ascii_lowercase();
        self.list
            .iter()
            .find(|category| category.extensions.contains(&extension))
            .map_or(CategoryId::OTHER, |category| category.id)
    }

    /// Where a file with this name is saved when sorting by category.
    pub fn save_folder_for(&self, file_name: &str, download_dir: &Path) -> PathBuf {
        self.get(self.classify(file_name)).save_folder(download_dir)
    }

    pub fn add(&mut self, name: &str, look: CategoryLook) -> Result<CategoryId, CategoryError> {
        let name = self.checked_name(name, None)?;
        let id = CategoryId(
            self.list
                .iter()
                .map(|category| category.id.0)
                .max()
                .unwrap_or(0)
                + 1,
        );
        // New categories go before Other, which stays last.
        let position = self.list.len().saturating_sub(1);
        self.list.insert(
            position,
            Category {
                id,
                name,
                extensions: Vec::new(),
                folder: None,
                look,
            },
        );
        Ok(id)
    }

    pub fn remove(&mut self, id: CategoryId) -> Result<(), CategoryError> {
        if id == CategoryId::OTHER {
            return Err(CategoryError::OtherIsPermanent);
        }
        let before = self.list.len();
        self.list.retain(|category| category.id != id);
        if self.list.len() == before {
            return Err(CategoryError::Unknown(id));
        }
        Ok(())
    }

    /// Replaces a category's editable fields. An extension moves here from
    /// any other category that listed it, so each extension has one home.
    pub fn update(&mut self, edited: Category) -> Result<(), CategoryError> {
        let name = self.checked_name(&edited.name, Some(edited.id))?;
        let position = self
            .list
            .iter()
            .position(|category| category.id == edited.id)
            .ok_or(CategoryError::Unknown(edited.id))?;
        for category in &mut self.list {
            if category.id != edited.id {
                category
                    .extensions
                    .retain(|extension| !edited.extensions.contains(extension));
            }
        }
        let folder = edited.folder.as_deref().map(crate::paths::native);
        self.list[position] = Category {
            name,
            folder,
            ..edited
        };
        Ok(())
    }

    fn checked_name(
        &self,
        name: &str,
        editing: Option<CategoryId>,
    ) -> Result<String, CategoryError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CategoryError::EmptyName);
        }
        let taken = self.list.iter().any(|category| {
            Some(category.id) != editing && category.name.eq_ignore_ascii_case(name)
        });
        if taken {
            return Err(CategoryError::DuplicateName(name.to_owned()));
        }
        Ok(name.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension_case_insensitively() {
        let categories = Categories::default();
        let name = |file| categories.get(categories.classify(file)).name.clone();
        assert_eq!(name("report.PDF"), "Documents");
        assert_eq!(name("setup.exe"), "Programs");
        assert_eq!(name("movie.final.mkv"), "Video");
        assert_eq!(name("README"), "Other");
    }

    #[test]
    fn added_category_takes_extensions_from_others() {
        let mut categories = Categories::default();
        let id = categories.add("Images", CategoryLook::Image).unwrap();
        let mut images = categories.get(id).clone();
        images.extensions = parse_extensions("png, .JPG webp md");
        categories.update(images).unwrap();
        assert_eq!(categories.classify("photo.jpg"), id);
        // "md" moved from Documents to Images.
        assert_eq!(categories.classify("notes.md"), id);
        assert_eq!(categories.all().last().unwrap().id, CategoryId::OTHER);
    }

    #[test]
    fn other_is_permanent_and_names_are_unique() {
        let mut categories = Categories::default();
        assert_eq!(
            categories.remove(CategoryId::OTHER),
            Err(CategoryError::OtherIsPermanent)
        );
        assert_eq!(
            categories.add(" video ", CategoryLook::Video),
            Err(CategoryError::DuplicateName("video".into()))
        );
        assert_eq!(
            categories.add("  ", CategoryLook::Generic),
            Err(CategoryError::EmptyName)
        );
        let video = categories.classify("a.mp4");
        categories.remove(video).unwrap();
        assert_eq!(categories.classify("a.mp4"), CategoryId::OTHER);
    }

    #[test]
    fn saved_list_without_other_gets_it_back() {
        let saved =
            r#"[{"id":7,"name":"Books","extensions":["epub"],"folder":null,"look":"document"}]"#;
        let categories: Categories = serde_json::from_str(saved).unwrap();
        assert_eq!(categories.all().len(), 2);
        assert_eq!(categories.classify("x.epub"), CategoryId(7));
    }

    #[test]
    fn save_folder_defaults_under_download_dir() {
        let categories = Categories::default();
        assert_eq!(
            categories.save_folder_for("a.mp4", Path::new("D:/dl")),
            Path::new("D:/dl").join("Video")
        );
    }
}
