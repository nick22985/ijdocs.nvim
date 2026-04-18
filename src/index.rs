use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct JarEntry {
    pub path: PathBuf,
    pub artifact_id: String,
    pub maven_coord: Option<String>,
    pub packages: Vec<String>,
    pub classes_jar: Option<PathBuf>,
}

pub struct Index {
    jars: Vec<JarEntry>,
    by_package: HashMap<String, Vec<usize>>,
    by_artifact: RwLock<HashMap<String, usize>>,
}

impl Index {
    pub fn build(roots: &[PathBuf]) -> Result<Self> {
        let mut jars: Vec<JarEntry> = Vec::new();
        for root in roots {
            for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n,
                    None => continue,
                };
                if !name.ends_with("-javadoc.jar") {
                    continue;
                }
                match read_packages(path) {
                    Ok(packages) => {
                        let artifact_id = artifact_id_from_filename(name);
                        let maven_coord = maven_coord_from_path(path);
                        let classes_jar = find_classes_jar(path);
                        jars.push(JarEntry {
                            path: path.to_path_buf(),
                            artifact_id,
                            maven_coord,
                            packages,
                            classes_jar,
                        });
                    }
                    Err(e) => tracing::warn!(?path, error=%e, "failed to read jar"),
                }
            }
        }

        let mut by_package: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_artifact: HashMap<String, usize> = HashMap::new();
        for (idx, jar) in jars.iter().enumerate() {
            for pkg in &jar.packages {
                by_package.entry(pkg.clone()).or_default().push(idx);
            }
            by_artifact.insert(jar.artifact_id.clone(), idx);
        }

        Ok(Self {
            jars,
            by_package,
            by_artifact: RwLock::new(by_artifact),
        })
    }

    pub fn jar_count(&self) -> usize {
        self.jars.len()
    }
    pub fn class_count(&self) -> usize {
        self.by_package.values().map(|v| v.len()).sum()
    }

    pub fn jar(&self, idx: usize) -> Option<&JarEntry> {
        self.jars.get(idx)
    }

    pub fn jar_by_artifact(&self, artifact: &str) -> Option<(usize, &JarEntry)> {
        let idx = *self.by_artifact.read().get(artifact)?;
        Some((idx, &self.jars[idx]))
    }

    pub fn resolve(&self, fqn: &str) -> Option<(usize, String)> {
        self.resolve_with_hint(fqn, None)
    }

    pub fn resolve_with_hint(&self, fqn: &str, jar_hint: Option<&str>) -> Option<(usize, String)> {
        let parts: Vec<&str> = fqn.split('.').collect();
        if parts.len() < 2 {
            return None;
        }
        for split in (1..parts.len()).rev() {
            let pkg = parts[..split].join(".");
            let class_path = parts[split..].join("/");
            let entry_path = format!("{}/{}.html", pkg.replace('.', "/"), class_path);
            if let Some(jar_indices) = self.by_package.get(&pkg) {
                if let Some(hint) = jar_hint {
                    for &idx in jar_indices {
                        if self.jars[idx].artifact_id.contains(hint)
                            || hint.contains(&self.jars[idx].artifact_id)
                            || jar_filename_matches(&self.jars[idx].path, hint)
                        {
                            if jar_contains_entry(&self.jars[idx].path, &entry_path)
                                .unwrap_or(false)
                            {
                                return Some((idx, entry_path));
                            }
                        }
                    }
                }
                for &idx in jar_indices {
                    if jar_contains_entry(&self.jars[idx].path, &entry_path).unwrap_or(false) {
                        return Some((idx, entry_path));
                    }
                }
            }
        }
        None
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let q = query.to_lowercase();
        let mut hits = Vec::new();
        for (idx, jar) in self.jars.iter().enumerate() {
            for pkg in &jar.packages {
                if pkg.to_lowercase().contains(&q) {
                    hits.push(SearchHit {
                        kind: "package".into(),
                        fqn: pkg.clone(),
                        artifact: jar.artifact_id.clone(),
                        jar_idx: idx,
                    });
                    if hits.len() >= limit {
                        return hits;
                    }
                }
            }
        }
        hits
    }
}

#[derive(serde::Serialize)]
pub struct SearchHit {
    pub kind: String,
    pub fqn: String,
    pub artifact: String,
    pub jar_idx: usize,
}

fn artifact_id_from_filename(name: &str) -> String {
    name.trim_end_matches("-javadoc.jar").to_string()
}

fn maven_coord_from_path(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    let repo_marker = ".m2/repository/";
    let start = s.find(repo_marker)? + repo_marker.len();
    let rel = &s[start..];
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() < 3 {
        return None;
    }
    let version = parts[parts.len() - 2];
    let artifact = parts[parts.len() - 3];
    let group = parts[..parts.len() - 3].join(".");
    Some(format!("{group}:{artifact}:{version}"))
}

fn read_packages(path: &Path) -> Result<Vec<String>> {
    let file = File::open(path).with_context(|| format!("opening {path:?}"))?;
    let mut zip = zip::ZipArchive::new(file)?;
    for candidate in ["element-list", "package-list"] {
        if let Ok(mut f) = zip.by_name(candidate) {
            let mut s = String::new();
            f.read_to_string(&mut s)?;
            return Ok(parse_element_list(&s));
        }
    }
    let mut pkgs = Vec::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        let n = entry.name();
        if let Some(stripped) = n.strip_suffix("/package-summary.html") {
            pkgs.push(stripped.replace('/', "."));
        }
    }
    Ok(pkgs)
}

fn parse_element_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("module:") {
            continue;
        }
        out.push(line.to_string());
    }
    out
}

fn jar_filename_matches(jar_path: &Path, hint: &str) -> bool {
    let Some(name) = jar_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let base = name.strip_suffix("-javadoc.jar").unwrap_or(name);
    let hint_base = hint.strip_suffix(".jar").unwrap_or(hint);
    base == hint_base || base.contains(hint_base) || hint_base.contains(base)
}

fn find_classes_jar(javadoc_jar: &Path) -> Option<PathBuf> {
    let name = javadoc_jar.file_name()?.to_str()?;
    let base = name.strip_suffix("-javadoc.jar")?;
    let classes_name = format!("{base}.jar");
    let candidate = javadoc_jar.with_file_name(&classes_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    let parent = javadoc_jar.parent()?;
    let grandparent = parent.parent()?;
    for entry in std::fs::read_dir(grandparent).ok()? {
        let entry = entry.ok()?;
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let candidate = entry.path().join(&classes_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn jar_contains_entry(jar: &Path, entry: &str) -> Result<bool> {
    let file = File::open(jar)?;
    let mut zip = zip::ZipArchive::new(file)?;
    Ok(zip.by_name(entry).is_ok())
}
