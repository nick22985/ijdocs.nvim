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
    pub is_sources: bool,
}

pub struct Index {
    jars: Vec<JarEntry>,
    by_package: HashMap<String, Vec<usize>>,
    by_artifact: RwLock<HashMap<String, usize>>,
    by_class: Vec<ClassEntry>,
}

pub const JDK_JAR_IDX: usize = 999_999_999;

pub struct ClassEntry {
    pub class_name: String,
    pub fqn: String,
    pub jar_idx: usize,
}

impl Index {
    pub fn build(roots: &[PathBuf]) -> Result<Self> {
        let mut jars: Vec<JarEntry> = Vec::new();
        let mut covered_coords: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut source_candidates: Vec<(PathBuf, String)> = Vec::new();
        let mut classes_candidates: Vec<(PathBuf, String)> = Vec::new();

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
                if !name.ends_with(".jar") {
                    continue;
                }
                if name.ends_with("-javadoc.jar") {
                    match read_packages(path) {
                        Ok(packages) => {
                            let artifact_id = artifact_id_from_filename(name);
                            let maven_coord = maven_coord_from_path(path);
                            let classes_jar = find_classes_jar(path);
                            if let Some(coord) = &maven_coord {
                                let base = coord.rsplitn(2, ':').last().unwrap_or(coord);
                                covered_coords.insert(base.to_string());
                            }
                            jars.push(JarEntry {
                                path: path.to_path_buf(),
                                artifact_id,
                                maven_coord,
                                packages,
                                classes_jar,
                                is_sources: false,
                            });
                        }
                        Err(e) => tracing::warn!(?path, error=%e, "failed to read jar"),
                    }
                } else if name.ends_with("-sources.jar") {
                    if has_missing_javadoc(path, name) {
                        source_candidates.push((path.to_path_buf(), name.to_string()));
                    }
                } else if has_failed_javadoc_download(path) {
                    classes_candidates.push((path.to_path_buf(), name.to_string()));
                }
            }
        }

        for (path, name) in source_candidates {
            let maven_coord = maven_coord_from_path(&path);
            let coord_key = maven_coord
                .as_ref()
                .map(|c| c.rsplitn(2, ':').last().unwrap_or(c).to_string());
            if coord_key
                .as_ref()
                .map_or(false, |k| covered_coords.contains(k))
            {
                continue;
            }
            match read_source_packages(&path) {
                Ok(packages) if !packages.is_empty() => {
                    let artifact_id = name.trim_end_matches("-sources.jar").to_string();
                    let classes_jar = find_classes_jar_for_sources(&path);
                    if let Some(k) = &coord_key {
                        covered_coords.insert(k.clone());
                    }
                    jars.push(JarEntry {
                        path,
                        artifact_id,
                        maven_coord,
                        packages,
                        classes_jar,
                        is_sources: true,
                    });
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(?path, error=%e, "failed to read sources jar"),
            }
        }

        for (path, name) in classes_candidates {
            let maven_coord = maven_coord_from_path(&path);
            let coord_key = maven_coord
                .as_ref()
                .map(|c| c.rsplitn(2, ':').last().unwrap_or(c).to_string());
            if coord_key
                .as_ref()
                .map_or(false, |k| covered_coords.contains(k))
            {
                continue;
            }
            match read_classes_packages(&path) {
                Ok(packages) if !packages.is_empty() => {
                    let artifact_id = name.trim_end_matches(".jar").to_string();
                    if let Some(k) = &coord_key {
                        covered_coords.insert(k.clone());
                    }
                    jars.push(JarEntry {
                        path: path.clone(),
                        artifact_id,
                        maven_coord,
                        packages,
                        classes_jar: Some(path),
                        is_sources: false,
                    });
                }
                Ok(_) => {}
                Err(_) => {}
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

        let mut by_class = Vec::new();
        for (idx, jar) in jars.iter().enumerate() {
            let is_classes_only = jar.classes_jar.as_ref() == Some(&jar.path);
            let classes = if jar.is_sources {
                scan_source_classes(&jar.path).unwrap_or_default()
            } else if is_classes_only {
                scan_bytecode_classes(&jar.path).unwrap_or_default()
            } else {
                scan_all_classes(&jar.path).unwrap_or_default()
            };
            for (pkg, class_name) in classes {
                let fqn = format!("{pkg}.{class_name}");
                by_class.push(ClassEntry {
                    class_name,
                    fqn,
                    jar_idx: idx,
                });
            }
        }
        by_class.sort_by(|a, b| a.class_name.cmp(&b.class_name));

        Ok(Self {
            jars,
            by_package,
            by_artifact: RwLock::new(by_artifact),
            by_class,
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
            let class_parts = &parts[split..];
            let pkg_path = pkg.replace('.', "/");
            if let Some(jar_indices) = self.by_package.get(&pkg) {
                let try_jar = |idx: usize| -> Option<(usize, String)> {
                    let jar = &self.jars[idx];
                    let is_classes_only = jar.classes_jar.as_ref() == Some(&jar.path);
                    let candidates: Vec<String> = if jar.is_sources {
                        let mut c = vec![format!("{}/{}.java", pkg_path, class_parts[0])];
                        if class_parts.len() > 1 {
                            c.push(format!("{}/{}.java", pkg_path, class_parts.join("/")));
                        }
                        c
                    } else if is_classes_only {
                        let mut c = vec![format!("{}/{}.class", pkg_path, class_parts[0])];
                        if class_parts.len() > 1 {
                            c.push(format!(
                                "{}/{}${}.class",
                                pkg_path,
                                class_parts[0],
                                class_parts[1..].join("$")
                            ));
                        }
                        c
                    } else {
                        let mut c = vec![format!("{}/{}.html", pkg_path, class_parts.join("/"))];
                        if class_parts.len() > 1 {
                            c.push(format!("{}/{}.html", pkg_path, class_parts.join(".")));
                        }
                        c
                    };
                    for entry_path in &candidates {
                        if jar_contains_entry(&jar.path, entry_path).unwrap_or(false) {
                            return Some((idx, entry_path.clone()));
                        }
                    }
                    None
                };

                if let Some(hint) = jar_hint {
                    for &idx in jar_indices {
                        if self.jars[idx].artifact_id.contains(hint)
                            || hint.contains(&self.jars[idx].artifact_id)
                            || jar_filename_matches(&self.jars[idx].path, hint)
                        {
                            if let Some(result) = try_jar(idx) {
                                return Some(result);
                            }
                        }
                    }
                }
                for &idx in jar_indices {
                    if let Some(result) = try_jar(idx) {
                        return Some(result);
                    }
                }
            }
        }
        None
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let q = query.to_lowercase();
        let mut seen_fqns: HashMap<String, usize> = HashMap::new();
        let mut exact = Vec::new();
        let mut partial = Vec::new();

        for entry in &self.by_class {
            let cn_lower = entry.class_name.to_lowercase();
            let fqn_lower = entry.fqn.to_lowercase();
            if cn_lower.contains(&q) || fqn_lower.contains(&q) {
                if seen_fqns.contains_key(&entry.fqn) {
                    continue;
                }
                seen_fqns.insert(entry.fqn.clone(), 0);
                let artifact = self
                    .jars
                    .get(entry.jar_idx)
                    .map(|j| j.artifact_id.clone())
                    .unwrap_or_default();
                let hit = SearchHit {
                    kind: "class".into(),
                    fqn: entry.fqn.clone(),
                    artifact,
                    jar_idx: entry.jar_idx,
                };
                if cn_lower == q {
                    exact.push(hit);
                } else if cn_lower.starts_with(&q) {
                    exact.push(hit);
                } else {
                    partial.push(hit);
                }
            }
        }

        for (idx, jar) in self.jars.iter().enumerate() {
            for pkg in &jar.packages {
                if pkg.to_lowercase().contains(&q) && !seen_fqns.contains_key(pkg) {
                    seen_fqns.insert(pkg.clone(), 0);
                    partial.push(SearchHit {
                        kind: "package".into(),
                        fqn: pkg.clone(),
                        artifact: jar.artifact_id.clone(),
                        jar_idx: idx,
                    });
                }
            }
        }

        exact.extend(partial);
        exact.truncate(limit);
        exact
    }

    pub fn class_entries(&self) -> &[ClassEntry] {
        &self.by_class
    }

    pub fn add_classes(&mut self, entries: Vec<ClassEntry>) {
        self.by_class.extend(entries);
        self.by_class
            .sort_by(|a, b| a.class_name.cmp(&b.class_name));
    }

    pub fn list_artifacts(&self) -> Vec<ArtifactInfo> {
        let mut seen = HashMap::new();
        for (idx, jar) in self.jars.iter().enumerate() {
            let key = jar
                .maven_coord
                .clone()
                .unwrap_or_else(|| jar.artifact_id.clone());
            seen.entry(key).or_insert_with(|| ArtifactInfo {
                artifact: jar.artifact_id.clone(),
                maven_coord: jar.maven_coord.clone(),
                jar_idx: idx,
                packages: jar.packages.clone(),
            });
        }
        let mut result: Vec<ArtifactInfo> = seen.into_values().collect();
        result.sort_by(|a, b| {
            let a_label = a.maven_coord.as_deref().unwrap_or(&a.artifact);
            let b_label = b.maven_coord.as_deref().unwrap_or(&b.artifact);
            a_label.cmp(b_label)
        });
        result
    }

    pub fn list_classes(&self, jar_idx: usize, package: &str) -> Vec<String> {
        let jar = match self.jars.get(jar_idx) {
            Some(j) => j,
            None => return Vec::new(),
        };
        let prefix = format!("{}/", package.replace('.', "/"));
        let file = match File::open(&jar.path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let mut zip = match zip::ZipArchive::new(file) {
            Ok(z) => z,
            Err(_) => return Vec::new(),
        };
        let is_classes_only = jar.classes_jar.as_ref() == Some(&jar.path);
        let mut classes = Vec::new();
        for i in 0..zip.len() {
            let entry = match zip.by_index(i) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.name().to_string();
            if let Some(rest) = name.strip_prefix(&prefix) {
                if jar.is_sources {
                    if rest.ends_with(".java")
                        && !rest.contains('/')
                        && !rest.contains("module-info")
                        && !rest.contains("package-info")
                    {
                        let class_name = rest.strip_suffix(".java").unwrap();
                        classes.push(class_name.to_string());
                    }
                } else if is_classes_only {
                    if rest.ends_with(".class")
                        && !rest.contains('/')
                        && !rest.contains('$')
                        && !rest.contains("module-info")
                        && !rest.contains("package-info")
                    {
                        let class_name = rest.strip_suffix(".class").unwrap();
                        classes.push(class_name.to_string());
                    }
                } else if rest.ends_with(".html")
                    && !rest.contains('/')
                    && !rest.ends_with("-summary.html")
                    && !rest.starts_with("package-")
                    && !rest.starts_with("class-use")
                {
                    let class_name = rest.strip_suffix(".html").unwrap();
                    classes.push(class_name.to_string());
                }
            }
        }
        classes.sort();
        classes
    }
}

#[derive(serde::Serialize)]
pub struct SearchHit {
    pub kind: String,
    pub fqn: String,
    pub artifact: String,
    pub jar_idx: usize,
}

#[derive(serde::Serialize)]
pub struct ArtifactInfo {
    pub artifact: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maven_coord: Option<String>,
    pub jar_idx: usize,
    pub packages: Vec<String>,
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

fn scan_all_classes(jar_path: &Path) -> Result<Vec<(String, String)>> {
    let file = File::open(jar_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut classes = Vec::new();
    for i in 0..zip.len() {
        let entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        if !name.ends_with(".html") || name.contains("class-use/") {
            continue;
        }
        if let Some(base) = name.strip_suffix(".html") {
            if base.ends_with("-summary")
                || base.ends_with("-list")
                || base.ends_with("-tree")
                || base.ends_with("-form")
                || base.ends_with("help-doc")
                || base.ends_with("index-all")
                || base.ends_with("index")
                || base.ends_with("allclasses-noframe")
                || base.ends_with("allclasses")
                || base.starts_with("package-")
                || base.contains("src-html/")
            {
                continue;
            }
            if let Some(last_slash) = base.rfind('/') {
                let pkg = base[..last_slash].replace('/', ".");
                let class_name = &base[last_slash + 1..];
                if !class_name.is_empty()
                    && class_name
                        .chars()
                        .next()
                        .map_or(false, |c| c.is_uppercase())
                {
                    classes.push((pkg, class_name.to_string()));
                }
            }
        }
    }
    Ok(classes)
}

fn jar_contains_entry(jar: &Path, entry: &str) -> Result<bool> {
    let file = File::open(jar)?;
    let mut zip = zip::ZipArchive::new(file)?;
    Ok(zip.by_name(entry).is_ok())
}

fn read_source_packages(path: &Path) -> Result<Vec<String>> {
    let file = File::open(path).with_context(|| format!("opening {path:?}"))?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut pkgs = std::collections::HashSet::new();
    for i in 0..zip.len() {
        let entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        if !name.ends_with(".java") || name.contains("module-info") || name.contains("package-info")
        {
            continue;
        }
        if let Some(last_slash) = name.rfind('/') {
            let pkg = name[..last_slash].replace('/', ".");
            if !pkg.starts_with("META-INF") {
                pkgs.insert(pkg);
            }
        }
    }
    let mut result: Vec<String> = pkgs.into_iter().collect();
    result.sort();
    Ok(result)
}

fn scan_source_classes(jar_path: &Path) -> Result<Vec<(String, String)>> {
    let file = File::open(jar_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut classes = Vec::new();
    for i in 0..zip.len() {
        let entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        if !name.ends_with(".java") || name.contains("module-info") || name.contains("package-info")
        {
            continue;
        }
        if let Some(base) = name.strip_suffix(".java") {
            if let Some(last_slash) = base.rfind('/') {
                let pkg = base[..last_slash].replace('/', ".");
                let class_name = &base[last_slash + 1..];
                if !class_name.is_empty()
                    && class_name
                        .chars()
                        .next()
                        .map_or(false, |c| c.is_uppercase())
                    && !pkg.starts_with("META-INF")
                {
                    classes.push((pkg, class_name.to_string()));
                }
            }
        }
    }
    Ok(classes)
}

fn find_classes_jar_for_sources(sources_jar: &Path) -> Option<PathBuf> {
    let name = sources_jar.file_name()?.to_str()?;
    let base = name.strip_suffix("-sources.jar")?;
    let classes_name = format!("{base}.jar");
    let candidate = sources_jar.with_file_name(&classes_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

fn read_classes_packages(path: &Path) -> Result<Vec<String>> {
    let file = File::open(path).with_context(|| format!("opening {path:?}"))?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut pkgs = std::collections::HashSet::new();
    for i in 0..zip.len() {
        let entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        if !name.ends_with(".class")
            || name.contains('$')
            || name.contains("module-info")
            || name.contains("package-info")
        {
            continue;
        }
        if name.starts_with("META-INF/") || name.starts_with("BOOT-INF/") {
            continue;
        }
        if let Some(last_slash) = name.rfind('/') {
            let pkg = name[..last_slash].replace('/', ".");
            if pkg.contains('.') {
                pkgs.insert(pkg);
            }
        }
    }
    let mut result: Vec<String> = pkgs.into_iter().collect();
    result.sort();
    Ok(result)
}

fn scan_bytecode_classes(jar_path: &Path) -> Result<Vec<(String, String)>> {
    let file = File::open(jar_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut classes = Vec::new();
    for i in 0..zip.len() {
        let entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        if !name.ends_with(".class")
            || name.contains('$')
            || name.contains("module-info")
            || name.contains("package-info")
        {
            continue;
        }
        if name.starts_with("META-INF/") || name.starts_with("BOOT-INF/") {
            continue;
        }
        if let Some(base) = name.strip_suffix(".class") {
            if let Some(last_slash) = base.rfind('/') {
                let pkg = base[..last_slash].replace('/', ".");
                let class_name = &base[last_slash + 1..];
                if !class_name.is_empty()
                    && class_name
                        .chars()
                        .next()
                        .map_or(false, |c| c.is_uppercase())
                {
                    classes.push((pkg, class_name.to_string()));
                }
            }
        }
    }
    Ok(classes)
}

fn has_failed_javadoc_download(jar_path: &Path) -> bool {
    let name = match jar_path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    let base = name.strip_suffix(".jar").unwrap_or(name);
    let javadoc_last_updated = jar_path.with_file_name(format!("{base}-javadoc.jar.lastUpdated"));
    javadoc_last_updated.is_file()
}

fn has_missing_javadoc(jar_path: &Path, name: &str) -> bool {
    let base = name.strip_suffix("-sources.jar").unwrap_or(name);
    let javadoc_jar = jar_path.with_file_name(format!("{base}-javadoc.jar"));
    if javadoc_jar.is_file() {
        return false;
    }
    let javadoc_last_updated = jar_path.with_file_name(format!("{base}-javadoc.jar.lastUpdated"));
    if javadoc_last_updated.is_file() {
        return true;
    }
    let path_str = jar_path.to_str().unwrap_or("");
    path_str.contains(".m2/repository/")
}
