use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

pub struct JdkSrc {
    path: PathBuf,
    packages: HashMap<String, String>,
    pub java_version: Option<String>,
    classes: Vec<(String, String)>,
}

impl JdkSrc {
    pub fn discover() -> Option<Self> {
        if let Ok(java_home) = std::env::var("JAVA_HOME") {
            let src_zip = PathBuf::from(&java_home).join("lib/src.zip");
            if src_zip.is_file() {
                return Self::from_path(src_zip);
            }
        }
        if let Ok(output) = std::process::Command::new("java")
            .arg("-XshowSettings:properties")
            .arg("-version")
            .output()
        {
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stderr.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("java.home") {
                    if let Some(val) = trimmed.split('=').nth(1) {
                        let src_zip = PathBuf::from(val.trim()).join("lib/src.zip");
                        if src_zip.is_file() {
                            return Self::from_path(src_zip);
                        }
                    }
                }
            }
        }
        None
    }

    pub fn from_path(path: PathBuf) -> Option<Self> {
        let file = File::open(&path).ok()?;
        let mut zip = zip::ZipArchive::new(file).ok()?;
        let mut packages: HashMap<String, String> = HashMap::new();
        let mut classes = Vec::new();

        for i in 0..zip.len() {
            let entry = match zip.by_index(i) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.name().to_string();
            if !name.ends_with(".java")
                || name.contains("module-info")
                || name.contains("package-info")
            {
                continue;
            }
            if let Some(slash) = name.find('/') {
                let module = &name[..slash];
                let rest = &name[slash + 1..];
                if let Some(last_slash) = rest.rfind('/') {
                    let pkg = rest[..last_slash].replace('/', ".");
                    packages
                        .entry(pkg.clone())
                        .or_insert_with(|| module.to_string());
                    let class_name = rest[last_slash + 1..]
                        .strip_suffix(".java")
                        .unwrap_or("")
                        .to_string();
                    if !class_name.is_empty() {
                        classes.push((pkg, class_name));
                    }
                }
            }
        }

        classes.sort_by(|a, b| a.1.cmp(&b.1));

        tracing::info!(
            packages = packages.len(),
            classes = classes.len(),
            path = %path.display(),
            "indexed JDK src.zip"
        );
        let java_version = path
            .parent()
            .and_then(|lib| lib.parent())
            .and_then(|home| {
                let release = home.join("release");
                let content = std::fs::read_to_string(release).ok()?;
                for line in content.lines() {
                    if let Some(rest) = line.strip_prefix("JAVA_VERSION=") {
                        let ver = rest.trim_matches('"').trim();
                        return ver.split('.').next().map(|v| v.to_string());
                    }
                }
                None
            });

        Some(Self {
            path,
            packages,
            java_version,
            classes,
        })
    }

    pub fn package_list(&self) -> Vec<String> {
        let mut pkgs: Vec<String> = self.packages.keys().cloned().collect();
        pkgs.sort();
        pkgs
    }

    pub fn list_classes(&self, package: &str) -> Vec<String> {
        let module = match self.packages.get(package) {
            Some(m) => m,
            None => return Vec::new(),
        };
        let prefix = format!("{}/{}/", module, package.replace('.', "/"));
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let mut zip = match zip::ZipArchive::new(file) {
            Ok(z) => z,
            Err(_) => return Vec::new(),
        };
        let mut classes = Vec::new();
        for i in 0..zip.len() {
            let entry = match zip.by_index(i) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.name().to_string();
            if let Some(rest) = name.strip_prefix(&prefix) {
                if rest.ends_with(".java")
                    && !rest.contains('/')
                    && !rest.contains("module-info")
                    && !rest.contains("package-info")
                {
                    let class_name = rest.strip_suffix(".java").unwrap();
                    classes.push(class_name.to_string());
                }
            }
        }
        classes.sort();
        classes
    }

    pub fn all_classes(&self) -> &[(String, String)] {
        &self.classes
    }

    pub fn docs_url(&self, fqn: &str) -> Option<String> {
        let parts: Vec<&str> = fqn.split('.').collect();
        if parts.len() < 2 {
            return None;
        }
        for split in (1..parts.len()).rev() {
            let pkg = parts[..split].join(".");
            if let Some(module) = self.packages.get(&pkg) {
                let class_path = parts[split..].join("/");
                let ver = self.java_version.as_deref().unwrap_or("21");
                return Some(format!(
                    "https://docs.oracle.com/en/java/javase/{ver}/docs/api/{module}/{}/{class_path}.html",
                    pkg.replace('.', "/")
                ));
            }
        }
        None
    }

    pub fn read_source(&self, fqn: &str) -> Option<String> {
        let parts: Vec<&str> = fqn.split('.').collect();
        if parts.len() < 2 {
            return None;
        }
        for split in (1..parts.len()).rev() {
            let pkg = parts[..split].join(".");
            if let Some(module) = self.packages.get(&pkg) {
                let class_path = parts[split..].join("/");
                let entry = format!("{}/{}/{}.java", module, pkg.replace('.', "/"), class_path);
                if let Some(content) = self.read_zip_entry(&entry) {
                    return Some(content);
                }
                if parts.len() > split + 1 {
                    let outer = parts[split];
                    let outer_entry =
                        format!("{}/{}/{}.java", module, pkg.replace('.', "/"), outer);
                    if let Some(content) = self.read_zip_entry(&outer_entry) {
                        return Some(content);
                    }
                }
            }
        }
        None
    }

    fn read_zip_entry(&self, entry_name: &str) -> Option<String> {
        let file = File::open(&self.path).ok()?;
        let mut zip = zip::ZipArchive::new(file).ok()?;
        let mut content = String::new();
        zip.by_name(entry_name)
            .ok()?
            .read_to_string(&mut content)
            .ok()?;
        Some(content)
    }
}
