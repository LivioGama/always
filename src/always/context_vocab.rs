use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use regex::Regex;

use super::config::VocabConfig;

/// Context-aware vocabulary that dynamically extracts terms from project files
#[derive(Debug, Clone)]
pub struct ContextVocabulary {
    project_root: Option<PathBuf>,
    git_branch: Option<String>,
    git_commit: Option<String>,
    extracted_terms: HashSet<String>,
    config: VocabConfig,
}

impl ContextVocabulary {
    pub fn new(project_root: Option<PathBuf>) -> Self {
        Self::new_with_config(project_root, VocabConfig::default())
    }

    pub fn new_with_config(project_root: Option<PathBuf>, config: VocabConfig) -> Self {
        let mut vocab = Self {
            project_root,
            git_branch: None,
            git_commit: None,
            extracted_terms: HashSet::new(),
            config,
        };

        if let Some(root) = vocab.project_root.clone() {
            vocab.extract_git_info(&root);
            vocab.extract_project_terms(&root).ok();
        }

        vocab
    }

    fn extract_git_info(&mut self, root: &Path) {
        // Get current branch
        if let Ok(branch_output) = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(root)
            .output()
            && let Ok(branch) = String::from_utf8(branch_output.stdout)
        {
            self.git_branch = Some(branch.trim().to_string());
        }

        // Get current commit
        if let Ok(commit_output) = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(root)
            .output()
            && let Ok(commit) = String::from_utf8(commit_output.stdout)
        {
            self.git_commit = Some(commit.trim().to_string());
        }
    }

    fn extract_project_terms(&mut self, root: &Path) -> Result<()> {
        let mut terms = HashSet::new();

        // Extract from codemap.md if it exists
        let codemap_path = root.join("codemap.md");
        if codemap_path.exists()
            && let Ok(content) = std::fs::read_to_string(&codemap_path)
        {
            Self::extract_terms_from_markdown(&content, &mut terms, &self.config);
        }

        // Extract from file names using Walk if available, otherwise simple directory traversal
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(file_name) = path.file_stem() {
                        let name = file_name.to_string_lossy().to_string();
                        Self::extract_terms_from_identifier(&name, &mut terms, &self.config);
                    }

                    if path.extension().is_some_and(|ext| {
                        matches!(
                            ext.to_str(),
                            Some("rs") | Some("ts") | Some("tsx") | Some("js") | Some("jsx")
                        )
                    }) && let Ok(content) = std::fs::read_to_string(path)
                    {
                        Self::extract_terms_from_javascript(&content, &mut terms, &self.config);
                    }
                }
            }
        }

        self.extracted_terms = terms;
        Ok(())
    }

    fn extract_terms_from_markdown(
        content: &str,
        terms: &mut HashSet<String>,
        config: &VocabConfig,
    ) {
        // Extract code blocks
        let code_block_regex = Regex::new(r"```[\w]*\n([\s\S]*?)```").unwrap();
        for cap in code_block_regex.captures_iter(content) {
            if let Some(code) = cap.get(1) {
                Self::extract_terms_from_identifier(code.as_str(), terms, config);
            }
        }

        // Extract inline code
        let inline_code_regex = Regex::new(r"`([^`]+)`").unwrap();
        for cap in inline_code_regex.captures_iter(content) {
            if let Some(code) = cap.get(1) {
                Self::extract_terms_from_identifier(code.as_str(), terms, config);
            }
        }
    }

    fn extract_terms_from_identifier(
        text: &str,
        terms: &mut HashSet<String>,
        config: &VocabConfig,
    ) {
        // Split by common delimiters
        for part in text.split(&[
            '/', '-', '.', '_', ' ', '(', ')', '[', ']', '{', '}', ':', ';', ',',
        ]) {
            let part = part.trim();
            if part.len() >= config.min_term_length && part.len() <= config.max_term_length {
                // Filter out common words
                let part_lower = part.to_lowercase();
                if !config.common_words.contains(&part_lower) {
                    terms.insert(part.to_string());
                }
            }
        }
    }

    fn extract_terms_from_javascript(
        content: &str,
        terms: &mut HashSet<String>,
        config: &VocabConfig,
    ) {
        // Simple regex-based extraction for JS/TS
        let function_regex = Regex::new(r"function\s+(\w+)").unwrap();
        for cap in function_regex.captures_iter(content) {
            if let Some(name) = cap.get(1) {
                Self::extract_terms_from_identifier(name.as_str(), terms, config);
            }
        }

        let const_regex = Regex::new(r"(?:const|let|var)\s+(\w+)").unwrap();
        for cap in const_regex.captures_iter(content) {
            if let Some(name) = cap.get(1) {
                Self::extract_terms_from_identifier(name.as_str(), terms, config);
            }
        }

        let class_regex = Regex::new(r"class\s+(\w+)").unwrap();
        for cap in class_regex.captures_iter(content) {
            if let Some(name) = cap.get(1) {
                Self::extract_terms_from_identifier(name.as_str(), terms, config);
            }
        }
    }

    pub fn reload(&mut self) -> Result<()> {
        if let Some(ref root) = self.project_root {
            let root_clone = root.clone();
            self.extract_git_info(&root_clone);
            self.extract_project_terms(&root_clone)?;
        }
        Ok(())
    }

    pub fn get_context_corrections(&self) -> HashMap<String, String> {
        let mut corrections = HashMap::new();

        // Add context-aware corrections based on project terms
        for term in &self.extracted_terms {
            // Generate phonetic variations
            let phonetic = self.phonetic_approximation(term);
            if phonetic != *term {
                corrections.insert(phonetic, term.clone());
            }
        }

        corrections
    }

    fn phonetic_approximation(&self, term: &str) -> String {
        let mut result = term.to_string();

        // Common phonetic substitutions
        result = result.replace("ph", "f");
        result = result.replace("gh", "g");
        result = result.replace("tion", "shun");
        result = result.replace("sion", "zhun");
        result = result.replace("x", "ks");
        result = result.replace("q", "k");
        result = result.replace("c", "k");

        result
    }

    pub fn get_git_context(&self) -> (Option<String>, Option<String>) {
        (self.git_branch.clone(), self.git_commit.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_terms_from_identifier() {
        let mut terms = HashSet::new();
        ContextVocabulary::extract_terms_from_identifier(
            "my_function_name",
            &mut terms,
            &VocabConfig::default(),
        );
        assert!(terms.contains("my"));
        assert!(terms.contains("function"));
        assert!(terms.contains("name"));
    }

    #[test]
    fn phonetic_approximation_works() {
        let vocab = ContextVocabulary::new(None);
        assert_eq!(vocab.phonetic_approximation("phone"), "fone");
        assert_eq!(vocab.phonetic_approximation("question"), "kuesshun");
    }
}
