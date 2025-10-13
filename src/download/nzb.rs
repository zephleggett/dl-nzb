pub use nzb_rs::Nzb as NzbRs;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

use crate::error::{DlNzbError, NzbError};

type Result<T> = std::result::Result<T, DlNzbError>;

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

// Wrapper struct that provides the same interface as before
#[derive(Debug, Clone)]
pub struct Nzb {
    // Cache converted files for performance
    files: Vec<NzbFile>,
}

impl Nzb {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        content.parse()
    }

    fn parse_content(content: &str) -> Result<Self> {
        let inner = NzbRs::parse(content)
            .map_err(|e| NzbError::ParseError(format!("Failed to parse NZB: {}", e)))?;

        // Convert nzb-rs structures to our compatible structures
        let files = inner
            .files
            .iter()
            .map(|file| {
                let segments = file
                    .segments
                    .iter()
                    .map(|segment| NzbSegment {
                        bytes: segment.size as u64,
                        number: segment.number,
                        message_id: segment.message_id.clone(),
                    })
                    .collect();

                let groups = file
                    .groups
                    .iter()
                    .map(|group| NzbGroup {
                        name: group.clone(),
                    })
                    .collect();

                NzbFile {
                    poster: file.poster.clone(),
                    date: file.posted_at.timestamp() as u64,
                    subject: file.subject.clone(),
                    groups: NzbGroups { group: groups },
                    segments: NzbSegments { segment: segments },
                }
            })
            .collect();

        Ok(Nzb { files })
    }

    pub fn files(&self) -> &Vec<NzbFile> {
        &self.files
    }

    pub fn total_size(&self) -> u64 {
        self.files
            .iter()
            .flat_map(|file| &file.segments.segment)
            .map(|segment| segment.bytes)
            .sum()
    }

    pub fn total_segments(&self) -> usize {
        self.files
            .iter()
            .map(|file| file.segments.segment.len())
            .sum()
    }

    pub fn get_filename_from_subject(subject: &str) -> Option<String> {
        // Extract filename from subject line like: [1/9] - "filename.ext" yEnc (1/5202)
        // Handle both regular quotes and HTML entities (&quot;)
        let re = regex::Regex::new(r#"(?:&quot;|")([^"]+)(?:&quot;|")"#).ok()?;
        re.captures(subject)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }
}

impl FromStr for Nzb {
    type Err = DlNzbError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse_content(s)
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
