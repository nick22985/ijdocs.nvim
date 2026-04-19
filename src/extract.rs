use crate::bytecode::MethodAnnotations;
use anyhow::{Context, Result};
use scraper::{ElementRef, Html, Selector};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub fn read_jar_entry(jar: &Path, entry: &str) -> Result<Vec<u8>> {
    let f = File::open(jar).with_context(|| format!("opening {jar:?}"))?;
    let mut zip = zip::ZipArchive::new(f)?;
    let mut e = zip
        .by_name(entry)
        .with_context(|| format!("entry {entry} in {jar:?}"))?;
    let mut buf = Vec::with_capacity(e.size() as usize);
    e.read_to_end(&mut buf)?;
    Ok(buf)
}

pub struct MdResult {
    pub markdown: String,
    pub type_refs: HashMap<String, String>,
}

pub fn html_to_markdown(
    html_bytes: &[u8],
    member: Option<&str>,
    annotations: Option<&MethodAnnotations>,
    fqn: Option<&str>,
) -> MdResult {
    let html_str = String::from_utf8_lossy(html_bytes);
    let doc = Html::parse_document(&html_str);

    let chunk_html = if let Some(m) = member {
        match extract_member(&doc, m) {
            Some(h) => h,
            None => extract_class_overview(&doc),
        }
    } else {
        extract_class_overview(&doc)
    };

    let base_pkg = fqn.and_then(|f| f.rsplit_once('.')).map(|(pkg, _)| pkg);
    let mut type_refs = extract_type_refs(&chunk_html, base_pkg);
    let stripped = strip_nav_and_clutter(&chunk_html);
    let md = html2md::parse_html(&stripped);
    let mut md = tidy_markdown(&md);

    if let (Some(m), Some(annots)) = (member, annotations) {
        if let Some(param_annots) = annots.get(m) {
            md = inject_annotations(&md, param_annots);
            for params in param_annots {
                for ann in params {
                    let key = ann.display.trim_start_matches('@').to_string();
                    type_refs.entry(key).or_insert_with(|| ann.fqn.clone());
                }
            }
        }
    }

    MdResult {
        markdown: md,
        type_refs,
    }
}

fn extract_type_refs(html: &str, base_pkg: Option<&str>) -> HashMap<String, String> {
    let doc = Html::parse_fragment(html);
    let mut refs = HashMap::new();
    let Ok(sel) = Selector::parse("a[href]") else {
        return refs;
    };
    for a in doc.select(&sel) {
        let href = match a.value().attr("href") {
            Some(h) => h,
            None => continue,
        };
        let text: String = a.text().collect();
        let text = text.trim().to_string();
        if text.is_empty() || text.contains(' ') || text.len() > 80 {
            continue;
        }
        if let Some(fqn) = href_to_fqn(href, base_pkg) {
            refs.insert(text, fqn);
        }
    }
    refs
}

fn strip_version_prefix(s: &str) -> &str {
    if let Some(slash) = s.find('/') {
        let prefix = &s[..slash];
        if prefix.contains('.') && prefix.chars().next().map_or(false, |c| c.is_ascii_digit()) {
            return &s[slash + 1..];
        }
    }
    s
}

fn href_to_fqn(href: &str, base_pkg: Option<&str>) -> Option<String> {
    if href.starts_with("http://") || href.starts_with("https://") {
        if let Some(rest) = href
            .find("/api/")
            .map(|i| &href[i + 5..])
            .or_else(|| href.find("/docs/api/").map(|i| &href[i + 10..]))
        {
            let rest = strip_version_prefix(rest);
            let path = rest.split(['?', '#']).next()?;
            let path = path.strip_suffix(".html")?;
            return Some(path.replace('/', "."));
        }
        if href.contains("javadoc.io/doc/") {
            if let Some(rest) = href.find("javadoc.io/doc/").map(|i| &href[i + 15..]) {
                let parts: Vec<&str> = rest.splitn(4, '/').collect();
                if parts.len() == 4 {
                    let path = parts[3].split(['?', '#']).next()?;
                    let path = path.strip_suffix(".html")?;
                    return Some(path.replace('/', "."));
                }
            }
        }
        return None;
    }

    let path = href.split(['?', '#']).next()?;
    let path = path.strip_suffix(".html")?;

    if path.ends_with("-summary")
        || path.ends_with("-list")
        || path.contains("class-use/")
        || path.ends_with("-tree")
        || path.ends_with("-form")
        || path.ends_with("help-doc")
        || path.ends_with("index-all")
        || path.ends_with("allclasses-noframe")
        || path.ends_with("index")
        || path.contains("src-html/")
    {
        return None;
    }

    if path.contains("../") {
        if let Some(pkg) = base_pkg {
            let pkg_parts: Vec<&str> = pkg.split('.').collect();
            let mut depth = 0;
            let mut rest = path;
            while let Some(stripped) = rest.strip_prefix("../") {
                depth += 1;
                rest = stripped;
            }
            if depth <= pkg_parts.len() {
                let base: Vec<&str> = pkg_parts[..pkg_parts.len() - depth].to_vec();
                let mut fqn = base.join(".");
                if !fqn.is_empty() {
                    fqn.push('.');
                }
                fqn.push_str(&rest.replace('/', "."));
                return Some(fqn);
            }
        }
        let clean = path.trim_start_matches("../");
        if clean.contains('/') {
            return Some(clean.replace('/', "."));
        }
        return None;
    }

    if path.contains('/') {
        let class_fqn = path.replace('/', ".");
        if let Some(pkg) = base_pkg {
            return Some(format!("{pkg}.{class_fqn}"));
        }
        return Some(class_fqn);
    }

    if let Some(pkg) = base_pkg {
        if path.chars().next().map_or(false, |c| c.is_uppercase()) {
            return Some(format!("{pkg}.{path}"));
        }
    }

    None
}

fn member_id_matches(id: &str, member: &str) -> bool {
    id == member || id.starts_with(&format!("{member}(")) || id.starts_with(&format!("{member}-"))
}

fn extract_member(doc: &Html, member: &str) -> Option<String> {
    if let Ok(sel) = Selector::parse("section.detail") {
        for sec in doc.select(&sel) {
            if let Some(id) = sec.value().attr("id") {
                if member_id_matches(id, member) {
                    return Some(sec.html());
                }
            }
        }
    }
    let sels = ["a[id]", "a[name]"];
    for s in sels {
        if let Ok(sel) = Selector::parse(s) {
            for a in doc.select(&sel) {
                let id = match a.value().attr("id").or_else(|| a.value().attr("name")) {
                    Some(id) => id,
                    None => continue,
                };
                if member_id_matches(id, member) {
                    let mut sib = a.next_siblings();
                    while let Some(node) = sib.next() {
                        if let Some(el) = ElementRef::wrap(node) {
                            if el.value().name() == "ul" || el.value().name() == "li" {
                                return Some(el.html());
                            }
                        }
                    }
                    if let Some(parent) = a.parent().and_then(ElementRef::wrap) {
                        return Some(parent.html());
                    }
                }
            }
        }
    }
    None
}

fn extract_class_overview(doc: &Html) -> String {
    let mut parts = Vec::new();

    let sig_sels = [
        "div.type-signature",
        "pre.type-signature",
        "section.class-description > .type-signature",
        ".typeNameLabel",
    ];
    for s in sig_sels {
        if let Ok(sel) = Selector::parse(s) {
            if let Some(el) = doc.select(&sel).next() {
                parts.push(format!("<pre>{}</pre>", el.text().collect::<String>()));
                break;
            }
        }
    }

    let desc_sels = [
        "section.class-description",
        ".description .block",
        "section.description",
        ".contentContainer .description .block",
        ".class-description",
    ];
    for s in desc_sels {
        if let Ok(sel) = Selector::parse(s) {
            if let Some(el) = doc.select(&sel).next() {
                parts.push(el.html());
                break;
            }
        }
    }

    if parts.is_empty() {
        if let Ok(sel) = Selector::parse("body > hr ~ p, .block > p, p.block") {
            let collected: Vec<String> = doc.select(&sel).take(6).map(|e| e.html()).collect();
            if !collected.is_empty() {
                parts.push(collected.join("\n"));
            }
        }
    }

    if parts.is_empty() {
        return "_(no description extracted — try the browser view: `gK`)_".to_string();
    }
    parts.join("\n\n")
}

fn strip_nav_and_clutter(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    let strip_selectors = [
        "header",
        "nav",
        ".topNav",
        ".bottomNav",
        ".subNav",
        ".skipNav",
        ".navList",
        ".aboutLanguage",
        ".legalCopy",
        ".fixedNav",
        "div.header",
        ".inheritance",
        ".sub-nav",
        "a.skip-nav",
        "[role=navigation]",
        "table[summary*='Navigation']",
    ];
    let mut to_strip = Vec::new();
    for s in strip_selectors {
        if let Ok(sel) = Selector::parse(s) {
            for el in doc.select(&sel) {
                to_strip.push(el.id());
            }
        }
    }
    serialize_filtered(&doc, &to_strip)
}

fn serialize_filtered(doc: &Html, strip: &[ego_tree::NodeId]) -> String {
    use std::collections::HashSet;
    let strip: HashSet<_> = strip.iter().copied().collect();
    let mut out = String::new();
    walk(doc.root_element(), &strip, &mut out);
    out
}

fn walk(el: ElementRef, strip: &std::collections::HashSet<ego_tree::NodeId>, out: &mut String) {
    for child in el.children() {
        if strip.contains(&child.id()) {
            continue;
        }
        if let Some(child_el) = ElementRef::wrap(child) {
            let name = child_el.value().name();
            out.push('<');
            out.push_str(name);
            for (k, v) in child_el.value().attrs() {
                if matches!(k, "href" | "id" | "class" | "src" | "alt" | "title") {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&v.replace('"', "&quot;"));
                    out.push('"');
                }
            }
            out.push('>');
            walk(child_el, strip, out);
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        } else if let Some(t) = child.value().as_text() {
            out.push_str(
                &t.text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            );
        }
    }
}

pub fn source_to_markdown(source: &str, member: Option<&str>) -> MdResult {
    let mut class_doc = None;
    let mut class_sig = None;
    let mut member_docs: Vec<(String, String, String)> = Vec::new();

    let mut current_doc: Option<String> = None;
    let mut in_comment = false;
    let mut comment_buf = String::new();

    let mut imports: HashMap<String, String> = HashMap::new();
    let mut package = String::new();

    for line in source.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("/**") {
            in_comment = true;
            comment_buf.clear();
            let rest = trimmed.strip_prefix("/**").unwrap_or("").trim();
            if rest.ends_with("*/") {
                let rest = rest.strip_suffix("*/").unwrap_or(rest).trim();
                comment_buf.push_str(rest);
                in_comment = false;
                current_doc = Some(comment_buf.clone());
            } else if !rest.is_empty() {
                comment_buf.push_str(rest);
                comment_buf.push('\n');
            }
            continue;
        }

        if in_comment {
            if trimmed.contains("*/") {
                let before = trimmed.split("*/").next().unwrap_or("").trim();
                let before = before.strip_prefix("*").unwrap_or(before).trim();
                if !before.is_empty() {
                    comment_buf.push_str(before);
                    comment_buf.push('\n');
                }
                in_comment = false;
                current_doc = Some(comment_buf.clone());
            } else {
                let content = trimmed.strip_prefix("*").unwrap_or(trimmed).trim_start();
                if content.is_empty() {
                    comment_buf.push('\n');
                } else {
                    comment_buf.push_str(content);
                    comment_buf.push('\n');
                }
            }
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("@") {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("package ") {
            package = rest.trim_end_matches(';').trim().to_string();
            current_doc = None;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("import ") {
            let imp = rest.trim_end_matches(';').trim();
            if !imp.starts_with("static ") && !imp.ends_with('*') {
                if let Some(short) = imp.rsplit('.').next() {
                    imports.insert(short.to_string(), imp.to_string());
                }
            }
            current_doc = None;
            continue;
        }

        if (trimmed.contains("class ")
            || trimmed.contains("interface ")
            || trimmed.contains("enum "))
            && !trimmed.starts_with("return")
            && class_sig.is_none()
        {
            let sig = trimmed.trim_end_matches('{').trim().to_string();
            for keyword in ["class ", "interface ", "enum "] {
                if let Some(pos) = sig.find(keyword) {
                    let after = &sig[pos + keyword.len()..];
                    let class_name = after
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .next()
                        .unwrap_or("");
                    if !class_name.is_empty() {
                        let fqn = if package.is_empty() {
                            class_name.to_string()
                        } else {
                            format!("{}.{}", package, class_name)
                        };
                        imports.insert(class_name.to_string(), fqn);
                    }
                    break;
                }
            }
            class_sig = Some(sig);
            class_doc = current_doc.take();
            continue;
        }

        if trimmed.contains('(') && class_sig.is_some() {
            let sig = trimmed
                .trim_end_matches('{')
                .trim_end_matches(';')
                .trim()
                .to_string();
            if let Some(paren_pos) = sig.find('(') {
                let before_paren = &sig[..paren_pos];
                let name = before_paren
                    .split_whitespace()
                    .last()
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() && name.chars().next().map_or(false, |c| c.is_alphabetic()) {
                    let doc = current_doc.take().unwrap_or_default();
                    member_docs.push((name, sig, doc));
                    continue;
                }
            }
        }

        current_doc = None;
    }

    if let Some(m) = member {
        for (name, sig, doc) in &member_docs {
            if name == m {
                let mut md = format!("{sig}\n\n");
                let mut type_refs = HashMap::new();
                if !doc.is_empty() {
                    let (doc_md, refs) = javadoc_to_markdown(doc, &imports, &package);
                    md.push_str(&doc_md);
                    type_refs = refs;
                }
                return MdResult {
                    markdown: md,
                    type_refs,
                };
            }
        }
        for (_name, sig, doc) in &member_docs {
            if let Some(paren) = sig.find('(') {
                let params_str = &sig[paren..];
                if params_str.contains(&format!(" {m},"))
                    || params_str.contains(&format!(" {m})"))
                    || params_str.contains(&format!(" {m} "))
                {
                    let param_info = extract_param_type(params_str, m);
                    let mut md = format!("{sig}\n\n");
                    if let Some(info) = param_info {
                        md = format!("{info}\n\n{sig}\n\n");
                    }
                    let mut type_refs = HashMap::new();
                    if !doc.is_empty() {
                        let (doc_md, refs) = javadoc_to_markdown(doc, &imports, &package);
                        md.push_str(&doc_md);
                        type_refs = refs;
                    }
                    return MdResult {
                        markdown: md,
                        type_refs,
                    };
                }
            }
        }
    }

    let mut md = String::new();
    let mut type_refs = HashMap::new();
    if let Some(sig) = &class_sig {
        md.push_str(&format!("{sig}\n\n"));
    }
    if let Some(doc) = &class_doc {
        let (doc_md, refs) = javadoc_to_markdown(doc, &imports, &package);
        md.push_str(&doc_md);
        type_refs = refs;
    } else {
        md.push_str("_(no javadoc found)_\n");
    }

    MdResult {
        markdown: md,
        type_refs,
    }
}

fn extract_param_type(params_str: &str, param_name: &str) -> Option<String> {
    let inner = params_str.trim_start_matches('(').trim_end_matches(')');
    for param in inner.split(',') {
        let param = param.trim();
        if param.ends_with(param_name) || param.contains(&format!(" {param_name}")) {
            return Some(param.to_string());
        }
    }
    None
}

fn process_inline_tags(
    doc: &str,
    imports: &HashMap<String, String>,
    package: &str,
) -> (String, HashMap<String, String>) {
    let mut type_refs = HashMap::new();
    let mut result = String::with_capacity(doc.len());
    let chars: Vec<char> = doc.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '{' && chars[i + 1] == '@' {
            i += 2;
            let mut tag = String::new();
            while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '}' {
                tag.push(chars[i]);
                i += 1;
            }
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            let mut content = String::new();
            let mut depth = 1;
            while i < chars.len() {
                if chars[i] == '{' {
                    depth += 1;
                }
                if chars[i] == '}' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                content.push(chars[i]);
                i += 1;
            }
            let content = content.split_whitespace().collect::<Vec<_>>().join(" ");

            match tag.as_str() {
                "code" => {
                    result.push('`');
                    result.push_str(content.trim());
                    result.push('`');
                }
                "link" | "linkplain" => {
                    let content = content.trim();
                    let has_explicit_display = content.contains(' ');
                    let (fqn_part, display) = parse_link_content(content);
                    let resolved = if !fqn_part.is_empty() {
                        resolve_link_fqn(&fqn_part, imports, package)
                    } else {
                        display.clone()
                    };
                    let render_text = if has_explicit_display {
                        display
                    } else {
                        resolved.clone()
                    };
                    type_refs
                        .entry(resolved.clone())
                        .or_insert(resolved.clone());
                    if has_explicit_display && render_text != resolved {
                        type_refs
                            .entry(render_text.clone())
                            .or_insert(resolved.clone());
                    }
                    if tag == "link" {
                        result.push('`');
                        result.push_str(&render_text);
                        result.push('`');
                    } else {
                        result.push_str(&render_text);
                    }
                }
                "literal" => {
                    result.push_str(content.trim());
                }
                "value" => {
                    result.push('`');
                    result.push_str(content.trim());
                    result.push('`');
                }
                "inheritDoc" | "docRoot" => {}
                _ => {
                    result.push_str(&content);
                }
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    (result, type_refs)
}

fn parse_link_content(content: &str) -> (String, String) {
    if let Some((fqn, display)) = content.split_once(' ') {
        (fqn.trim().to_string(), display.trim().to_string())
    } else if content.starts_with('#') {
        (content.to_string(), content[1..].to_string())
    } else if let Some(hash) = content.find('#') {
        let class = &content[..hash];
        let method = &content[hash + 1..];
        (content.to_string(), format!("{class}.{method}"))
    } else {
        (content.to_string(), content.to_string())
    }
}

fn resolve_link_fqn(raw: &str, imports: &HashMap<String, String>, package: &str) -> String {
    let (class_part, _method) = if raw.starts_with('#') {
        let _method = &raw[1..];
        for (_, fqn) in imports.iter() {
            if fqn.starts_with(package)
                && fqn.len() > package.len()
                && fqn[package.len()..].starts_with('.')
                && !fqn[package.len() + 1..].contains('.')
            {
                return fqn.clone();
            }
        }
        return raw.replace('#', ".");
    } else if let Some(hash) = raw.find('#') {
        (&raw[..hash], Some(&raw[hash + 1..]))
    } else {
        (raw, None)
    };

    if class_part.contains('.') {
        let parts: Vec<&str> = class_part.split('.').collect();
        if parts.len() >= 2 && parts[0].chars().next().map_or(false, |c| c.is_lowercase()) {
            return class_part.to_string();
        }
    }

    let base = class_part.split('.').next().unwrap_or(class_part);
    if let Some(fqn) = imports.get(base) {
        if class_part.contains('.') {
            let rest = &class_part[base.len()..];
            return format!("{fqn}{rest}");
        }
        return fqn.clone();
    }

    let java_lang_types = [
        "Object",
        "String",
        "Class",
        "System",
        "Thread",
        "Runnable",
        "Integer",
        "Long",
        "Double",
        "Float",
        "Boolean",
        "Byte",
        "Short",
        "Character",
        "Number",
        "Math",
        "Comparable",
        "Iterable",
        "Enum",
        "Record",
        "Exception",
        "RuntimeException",
        "Error",
        "Throwable",
        "IllegalArgumentException",
        "IllegalStateException",
        "NullPointerException",
        "IndexOutOfBoundsException",
        "UnsupportedOperationException",
        "ClassCastException",
        "ArithmeticException",
        "SecurityException",
        "ClassNotFoundException",
        "StringBuilder",
        "StringBuffer",
        "CharSequence",
        "Override",
        "Deprecated",
        "SuppressWarnings",
        "FunctionalInterface",
        "AutoCloseable",
        "Cloneable",
        "Process",
        "ProcessBuilder",
        "Runtime",
    ];
    if java_lang_types.contains(&base) {
        if class_part.contains('.') {
            let rest = &class_part[base.len()..];
            return format!("java.lang.{base}{rest}");
        }
        return format!("java.lang.{base}");
    }

    if !package.is_empty() {
        return format!("{package}.{class_part}");
    }

    class_part.to_string()
}

fn javadoc_to_markdown(
    doc: &str,
    imports: &HashMap<String, String>,
    package: &str,
) -> (String, HashMap<String, String>) {
    let (doc, type_refs) = process_inline_tags(doc, imports, package);

    let mut body_lines = Vec::new();
    let mut params = Vec::new();
    let mut returns = None;
    let mut throws = Vec::new();

    for line in doc.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("@param ") {
            params.push(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@return ") {
            returns = Some(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@throws ") {
            throws.push(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@exception ") {
            throws.push(rest.to_string());
        } else if trimmed.starts_with("@since")
            || trimmed.starts_with("@see")
            || trimmed.starts_with("@deprecated")
            || trimmed.starts_with("@author")
            || trimmed.starts_with("@version")
        {
            let tag = &trimmed[1..];
            if let Some((tag_name, rest)) = tag.split_once(' ') {
                body_lines.push(format!("\n**@{tag_name}** {rest}"));
            }
        } else if !trimmed.starts_with('@') {
            body_lines.push(line.to_string());
        }
    }

    let body = body_lines.join("\n");
    let mut out = convert_javadoc_html(&body);

    if !params.is_empty() {
        out.push_str("\n\n**Parameters:**\n");
        for p in &params {
            let p = convert_javadoc_html(p);
            let p = p.trim();
            if let Some((name, desc)) = p.split_once(' ') {
                out.push_str(&format!("- `{name}` — {desc}\n"));
            } else {
                out.push_str(&format!("- `{p}`\n"));
            }
        }
    }
    if let Some(ret) = &returns {
        let ret = convert_javadoc_html(ret);
        out.push_str(&format!("\n**Returns:** {}\n", ret.trim()));
    }
    if !throws.is_empty() {
        out.push_str("\n**Throws:**\n");
        for t in &throws {
            let t = convert_javadoc_html(t);
            let t = t.trim();
            if let Some((name, desc)) = t.split_once(' ') {
                out.push_str(&format!("- `{name}` — {desc}\n"));
            } else {
                out.push_str(&format!("- `{t}`\n"));
            }
        }
    }

    (out, type_refs)
}

fn convert_javadoc_html(s: &str) -> String {
    let mut s = s.to_string();
    s = s.replace("<p>", "\n\n");
    s = s.replace("</p>", "");
    s = s.replace("<em>", "*");
    s = s.replace("</em>", "*");
    s = s.replace("<i>", "*");
    s = s.replace("</i>", "*");
    s = s.replace("<b>", "**");
    s = s.replace("</b>", "**");
    s = s.replace("<strong>", "**");
    s = s.replace("</strong>", "**");
    s = s.replace("<code>", "`");
    s = s.replace("</code>", "`");
    s = s.replace("<tt>", "`");
    s = s.replace("</tt>", "`");
    s = s.replace("<ul>", "");
    s = s.replace("</ul>", "");
    s = s.replace("<ol>", "");
    s = s.replace("</ol>", "");
    s = s.replace("<li>", "\n- ");
    s = s.replace("</li>", "");
    s = s.replace("<pre>", "\n```\n");
    s = s.replace("</pre>", "\n```\n");
    s = s.replace("<blockquote>", "\n> ");
    s = s.replace("</blockquote>", "\n");
    s = s.replace("<br>", "\n");
    s = s.replace("<br/>", "\n");
    s = s.replace("<br />", "\n");
    s = s.replace("<sup>", "^");
    s = s.replace("</sup>", "");
    s = s.replace("<sub>", "_");
    s = s.replace("</sub>", "");
    s = strip_remaining_html_tags(&s);
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn strip_remaining_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' && in_tag {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

fn split_param_entries(s: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' && !current.is_empty() {
            let trimmed = current.trim();
            if trimmed.contains(" - ") || trimmed.contains(" — ") {
                entries.push(current.trim().to_string());
                current = String::new();
            }
        }
        current.push(chars[i]);
        i += 1;
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        entries.push(trimmed.to_string());
    }
    if entries.is_empty() && !s.trim().is_empty() {
        entries.push(s.trim().to_string());
    }
    entries
}

fn inject_annotations(md: &str, param_annots: &[Vec<crate::bytecode::Annotation>]) -> String {
    if param_annots.iter().all(|a| a.is_empty()) {
        return md.to_string();
    }

    let lines: Vec<&str> = md.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if lines[i].trim_start().starts_with("```") {
            result.push(lines[i].to_string());
            i += 1;
            let mut code_lines = Vec::new();
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code_lines.push(lines[i]);
                i += 1;
            }
            let combined = code_lines.join("\n");
            let flat = combined
                .replace('\n', " ")
                .replace("\u{00a0}", " ")
                .replace("\u{200b}", "");
            if let Some(injected) = try_inject_into_signature(&flat, param_annots) {
                result.push(injected);
            } else {
                result.push(combined);
            }
            if i < lines.len() {
                result.push(lines[i].to_string());
            }
        } else {
            result.push(lines[i].to_string());
        }
        i += 1;
    }

    result.join("\n")
}

fn try_inject_into_signature(
    line: &str,
    param_annots: &[Vec<crate::bytecode::Annotation>],
) -> Option<String> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close <= open + 1 {
        return None;
    }

    let before = &line[..open + 1];
    let params_str = &line[open + 1..close];
    let after = &line[close..];

    let params: Vec<&str> = split_java_params(params_str);
    if params.is_empty() {
        return None;
    }

    let mut any_injected = false;
    let mut new_params = Vec::new();
    for (i, param) in params.iter().enumerate() {
        let param = param.trim();
        if param.is_empty() {
            new_params.push(param.to_string());
            continue;
        }
        let annots = param_annots.get(i).cloned().unwrap_or_default();
        let display_annots: Vec<&str> = annots
            .iter()
            .filter(|a| !param.contains(a.display.as_str()))
            .map(|a| a.display.as_str())
            .collect();
        if display_annots.is_empty() {
            new_params.push(param.to_string());
        } else {
            let prefix = display_annots.join(" ");
            new_params.push(format!("{prefix} {param}"));
            any_injected = true;
        }
    }

    if !any_injected {
        return None;
    }

    Some(format!("{before}{}{after}", new_params.join(", ")))
}

fn split_java_params(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        match bytes[i] {
            b'<' => depth += 1,
            b'>' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            b',' if depth == 0 => {
                result.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    result.push(&s[start..]);
    result
}

fn strip_markdown_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']') {
                let close_bracket = i + 1 + close_bracket;
                if close_bracket + 1 < chars.len() && chars[close_bracket + 1] == '(' {
                    if let Some(close_paren) =
                        chars[close_bracket + 2..].iter().position(|&c| c == ')')
                    {
                        let close_paren = close_bracket + 2 + close_paren;
                        let text: String = chars[i + 1..close_bracket].iter().collect();
                        out.push_str(&text);
                        i = close_paren + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn tidy_markdown(md: &str) -> String {
    let md = strip_markdown_links(md);
    let md = md.replace('\u{200b}', "").replace('\u{00a0}', " ");
    let mut out = String::with_capacity(md.len());
    let mut blank_run = 0;
    for line in md.lines() {
        let trimmed = line.trim_end();
        let check = trimmed.trim_start_matches("* ");
        if check.starts_with("###") && check.ends_with("###") {
            continue;
        }
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push('\n');
            }
            continue;
        }
        blank_run = 0;
        let trimmed = trimmed.replace("[ ]()", "").replace("[]()", "");
        let trimmed = trimmed.trim();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push('\n');
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Parameters:") {
            out.push_str("\n**Parameters:**\n");
            if !rest.is_empty() {
                for param in split_param_entries(rest.trim()) {
                    out.push_str(&format!("- {param}\n"));
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("Throws:") {
            out.push_str("\n**Throws:**\n");
            if !rest.is_empty() {
                for param in split_param_entries(rest.trim()) {
                    out.push_str(&format!("- {param}\n"));
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("Returns:") {
            out.push_str("**Returns:** ");
            out.push_str(rest.trim());
            out.push('\n');
        } else if let Some(rest) = trimmed.strip_prefix("Since:") {
            out.push_str("**Since:** ");
            out.push_str(rest.trim());
            out.push('\n');
        } else if let Some(rest) = trimmed.strip_prefix("See Also:") {
            out.push_str("**See Also:** ");
            out.push_str(rest.trim());
            out.push('\n');
        } else {
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann(display: &str, fqn: &str) -> crate::bytecode::Annotation {
        crate::bytecode::Annotation {
            display: display.to_string(),
            fqn: fqn.to_string(),
        }
    }

    #[test]
    fn test_inject_annotations() {
        let md = "* #### registerEvents\n\n```\nvoid registerEvents(Listener listener, Plugin plugin)\n```\n\nRegisters all the events\n";
        let annots = vec![
            vec![ann("@NotNull", "org.jetbrains.annotations.NotNull")],
            vec![ann("@NotNull", "org.jetbrains.annotations.NotNull")],
        ];
        let result = inject_annotations(md, &annots);
        assert!(
            result.contains("@NotNull Listener listener"),
            "got: {result}"
        );
        assert!(result.contains("@NotNull Plugin plugin"), "got: {result}");
    }

    #[test]
    fn test_inject_no_duplicate() {
        let md = "```\nvoid foo(@Nullable String s)\n```\n";
        let annots = vec![vec![ann("@Nullable", "javax.annotation.Nullable")]];
        let result = inject_annotations(md, &annots);
        let count = result.matches("@Nullable").count();
        assert_eq!(count, 1, "should not duplicate: {result}");
    }

    #[test]
    fn test_inject_multiline_signature() {
        let md = "```\nvoid registerEvents(Listener listener,\nPlugin plugin)\n```\n";
        let annots = vec![
            vec![ann("@NotNull", "org.jetbrains.annotations.NotNull")],
            vec![ann("@NotNull", "org.jetbrains.annotations.NotNull")],
        ];
        let result = inject_annotations(md, &annots);
        assert!(
            result.contains("@NotNull Listener listener"),
            "got: {result}"
        );
        assert!(result.contains("@NotNull Plugin plugin"), "got: {result}");
    }
}
