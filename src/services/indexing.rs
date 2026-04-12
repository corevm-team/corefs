use crate::domain::metadata::ContentClass;

#[derive(Debug, Default)]
pub struct IndexingService;

impl IndexingService {
    pub fn classify_path(&self, path: &str) -> ContentClass {
        if path.ends_with(".txt") || path.ends_with(".md") {
            ContentClass::Text
        } else if path.ends_with(".png") || path.ends_with(".jpg") || path.ends_with(".jpeg") {
            ContentClass::Image
        } else if path.ends_with(".rs")
            || path.ends_with(".c")
            || path.ends_with(".cpp")
            || path.ends_with(".py")
        {
            ContentClass::SourceCode
        } else if path.ends_with(".zip") || path.ends_with(".tar") || path.ends_with(".gz") {
            ContentClass::Archive
        } else {
            ContentClass::Binary
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_path_detects_known_content_types() {
        let service = IndexingService;

        assert_eq!(service.classify_path("notes.txt"), ContentClass::Text);
        assert_eq!(service.classify_path("image.png"), ContentClass::Image);
        assert_eq!(service.classify_path("lib.rs"), ContentClass::SourceCode);
        assert_eq!(service.classify_path("archive.tar"), ContentClass::Archive);
        assert_eq!(service.classify_path("blob.bin"), ContentClass::Binary);
    }
}
