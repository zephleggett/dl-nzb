use serde::{Deserialize, Serialize};
use anyhow::Result;
use std::path::Path;
pub use nzb_rs::{Nzb as NzbRs};

// Re-export types for compatibility with existing code
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbSegment {
    pub bytes: u64,
    pub number: u32,
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbGroup {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbFile {
    pub poster: String,
    pub date: u64,
    pub subject: String,
    pub groups: NzbGroups,
    pub segments: NzbSegments,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbGroups {
    pub group: Vec<NzbGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbSegments {
    pub segment: Vec<NzbSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbHead {
    pub meta: Option<NzbMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbMeta {
    pub meta_type: String,
    pub value: String,
}

// Wrapper struct that provides the same interface as before
#[derive(Debug, Clone)]
pub struct Nzb {
    inner: NzbRs,
    // Cache converted files for performance
    files: Vec<NzbFile>,
}

impl Nzb {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_str(&content)
    }

    pub fn from_str(content: &str) -> Result<Self> {
        let inner = NzbRs::parse(content)
            .map_err(|e| anyhow::anyhow!("Failed to parse NZB: {}", e))?;

        // Convert nzb-rs structures to our compatible structures
        let files = inner.files.iter().map(|file| {
            let segments = file.segments.iter().map(|segment| {
                NzbSegment {
                    bytes: segment.size as u64,
                    number: segment.number,
                    message_id: segment.message_id.clone(),
                }
            }).collect();

            let groups = file.groups.iter().map(|group| {
                NzbGroup {
                    name: group.clone(),
                }
            }).collect();

            NzbFile {
                poster: file.poster.clone(),
                date: file.posted_at.timestamp() as u64,
                subject: file.subject.clone(),
                groups: NzbGroups { group: groups },
                segments: NzbSegments { segment: segments },
            }
        }).collect();

        Ok(Nzb { inner, files })
    }

    pub fn files(&self) -> &Vec<NzbFile> {
        &self.files
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter()
            .flat_map(|file| &file.segments.segment)
            .map(|segment| segment.bytes)
            .sum()
    }

    pub fn total_segments(&self) -> usize {
        self.files.iter()
            .map(|file| file.segments.segment.len())
            .sum()
    }

    pub fn get_filename_from_subject(subject: &str) -> Option<String> {
        // Extract filename from subject line like: [1/9] - "filename.ext" yEnc (1/5202)
        let re = regex::Regex::new(r#""([^"]+)""#).ok()?;
        re.captures(subject)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }

    pub fn get_main_files(&self) -> Vec<&NzbFile> {
        self.files.iter()
            .filter(|file| {
                let subject = &file.subject;
                // Filter out PAR2 files - everything else is considered main content
                !Self::is_par2_file(subject)
            })
            .collect()
    }

    pub fn get_par2_files(&self) -> Vec<&NzbFile> {
        self.files.iter()
            .filter(|file| Self::is_par2_file(&file.subject))
            .collect()
    }

    pub fn is_par2_file(subject: &str) -> bool {
        let subject_lower = subject.to_lowercase();
        subject_lower.contains(".par2") ||
        (subject_lower.contains(".par") && !subject_lower.contains(".part"))
    }

    // Additional methods that leverage nzb-rs capabilities
    pub fn inner(&self) -> &NzbRs {
        &self.inner
    }

    pub fn get_filename(&self) -> Option<String> {
        // Try to get filename from the first file's subject
        self.files.first()
            .and_then(|file| Self::get_filename_from_subject(&file.subject))
    }

    pub fn get_metadata(&self, key: &str) -> Option<String> {
        // nzb-rs has structured metadata, let's handle the common cases
        match key {
            "title" => self.inner.meta.title.clone(),
            "category" => self.inner.meta.category.clone(),
            _ => None,
        }
    }

    pub fn get_all_metadata(&self) -> Vec<(String, String)> {
        let mut metadata = Vec::new();

        if let Some(title) = &self.inner.meta.title {
            metadata.push(("title".to_string(), title.clone()));
        }

        if let Some(category) = &self.inner.meta.category {
            metadata.push(("category".to_string(), category.clone()));
        }

        for tag in &self.inner.meta.tags {
            metadata.push(("tag".to_string(), tag.clone()));
        }

        for password in &self.inner.meta.passwords {
            metadata.push(("password".to_string(), password.clone()));
        }

        metadata
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nzb_api_structure() {
        let xml = r#"
        <?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
        <nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
            <head>
                <meta type="title">Test File</meta>
            </head>
            <file poster="test@example.com" date="1234567890" subject="test.zip">
                <groups>
                    <group>alt.binaries.test</group>
                </groups>
                <segments>
                    <segment bytes="1024" number="1">test@example.com</segment>
                </segments>
            </file>
        </nzb>
        "#;

        let nzb_rs = NzbRs::parse(xml).unwrap();

        // Print the structure to understand the API
        println!("Files count: {}", nzb_rs.files.len());
        if let Some(file) = nzb_rs.files.first() {
            println!("File poster: {}", file.poster);
            println!("File posted_at: {:?}", file.posted_at);
            println!("File subject: {}", file.subject);
            if let Some(segment) = file.segments.first() {
                println!("Segment size: {}", segment.size);
                println!("Segment number: {}", segment.number);
                println!("Segment message_id: {}", segment.message_id);
            }
        }

        println!("Meta title: {:?}", nzb_rs.meta.title);
        println!("Meta category: {:?}", nzb_rs.meta.category);
    }
}
