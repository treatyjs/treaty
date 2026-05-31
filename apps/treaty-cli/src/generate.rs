//! `treaty generate <app|lib|component> <name>` — scaffold standalone,
//! signal-based, **selectorless** building blocks for a Treaty project.
//!
//! Treaty's compiler is the source of the Angular boilerplate, so the scaffold
//! deliberately omits it: the emitted sources carry **no `selector:`, no
//! `standalone: true`, and no signal plumbing**. A bare `@Component` class with
//! an inline template is enough — the render3 compiler derives the selector from
//! the class name, treats every component as standalone, and lowers field
//! initializers to signals. This keeps generated code minimal and matches the
//! "the compiler fills them in" principle.
//!
//! The generator is pure filesystem work — no bundler peer is involved — so it
//! runs with nothing installed but the CLI itself.
//!
//!   * `app`       -> a runnable standalone app: `treaty.config.json`,
//!     `index.html`, `src/main.ts` bootstrapping a root component.
//!   * `lib`       -> a sharable library with a public `src/index.ts` barrel and
//!     a sample component to expose as a remote.
//!   * `component` -> a single component file under `src/app`.

use std::path::PathBuf;

/// The kinds of artifact [`run_generate`] can scaffold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerateKind {
    App,
    Lib,
    Component,
}

impl GenerateKind {
    /// Parse a raw kind token.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "app" => Some(GenerateKind::App),
            "lib" => Some(GenerateKind::Lib),
            "component" => Some(GenerateKind::Component),
            _ => None,
        }
    }

    /// The token form.
    pub fn as_str(self) -> &'static str {
        match self {
            GenerateKind::App => "app",
            GenerateKind::Lib => "lib",
            GenerateKind::Component => "component",
        }
    }
}

/// Options controlling a scaffold run.
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    pub kind: GenerateKind,
    /// The artifact name (any case; used for paths + class names).
    pub name: String,
    /// The directory the artifact is written under.
    pub cwd: PathBuf,
    /// When true, plan the files but do not write them.
    pub dry_run: bool,
    /// When true, overwrite existing files instead of skipping them.
    pub force: bool,
}

/// A single file the generator plans to (or did) write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    pub path: PathBuf,
    pub contents: String,
}

/// The outcome of a scaffold run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateResult {
    pub kind: GenerateKind,
    pub name: String,
    /// Every file produced by the plan (whether or not it was written).
    pub files: Vec<GeneratedFile>,
    /// Paths actually written to disk (empty for a dry run).
    pub written: Vec<PathBuf>,
    /// Paths skipped because they already existed (and `force` was off).
    pub skipped: Vec<PathBuf>,
}

/// Convert a kebab/snake/space name to a PascalCase identifier base.
pub fn to_pascal_case(name: &str) -> String {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Convert a name to a kebab-case path-safe token.
pub fn to_kebab_case(name: &str) -> String {
    // Split camelCase boundaries first, then normalize separators.
    let mut spaced = String::with_capacity(name.len() * 2);
    let mut prev: Option<char> = None;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if let Some(p) = prev {
                if p.is_ascii_lowercase() || p.is_ascii_digit() {
                    spaced.push('-');
                }
            }
        }
        spaced.push(c);
        prev = Some(c);
    }
    let lowered = spaced.to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_dash = true; // suppress leading dash
    for c in lowered.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Render a **selectorless**, standalone, signal-based component source.
///
/// Intentionally minimal: no `selector:`, no `standalone:` flag, no `signal()`
/// imports. The render3 compiler derives the selector from the class name, makes
/// the component standalone, and lowers the field initializer to a signal.
fn component_source(name: &str) -> String {
    let class_name = format!("{}Component", to_pascal_case(name));
    let token = to_kebab_case(name);
    format!(
        "import {{ Component }} from '@angular/core'\n\
         \n\
         // {class_name} — a Treaty component. No selector / standalone / signal\n\
         // boilerplate: the compiler derives the selector from the class name,\n\
         // treats the component as standalone, and lowers fields to signals.\n\
         @Component({{\n  \
           template: `<p>{{{{ title }}}}</p>`,\n\
         }})\n\
         export class {class_name} {{\n  \
           // A plain field initializer; the compiler lowers it to a signal.\n  \
           title = '{token} works'\n\
         }}\n"
    )
}

/// Render the root standalone bootstrap for a generated app.
fn main_source(name: &str) -> String {
    let class_name = format!("{}Component", to_pascal_case(name));
    let token = to_kebab_case(name);
    format!(
        "import {{ bootstrapApplication }} from '@angular/platform-browser'\n\
         import {{ {class_name} }} from './app/{token}.component'\n\
         \n\
         // Standalone bootstrap — no NgModule. The compiled app is automatically\n\
         // a Module Federation host (Treaty wires this; you configure nothing).\n\
         void bootstrapApplication({class_name})\n"
    )
}

/// Render the `index.html` shell. The custom element tag uses the compiler's
/// derived selector convention (`app-<kebab>`).
fn index_html_source(name: &str) -> String {
    let token = to_kebab_case(name);
    format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n  \
           <head>\n    \
             <meta charset=\"utf-8\" />\n    \
             <title>{name}</title>\n    \
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n  \
           </head>\n  \
           <body>\n    \
             <app-{token}></app-{token}>\n    \
             <script type=\"module\" src=\"/src/main.ts\"></script>\n  \
           </body>\n\
         </html>\n"
    )
}

/// Render a minimal `treaty.config.json` for a generated app. Federation is on
/// by default; the file only names the app.
fn treaty_config_source(name: &str) -> String {
    let token = to_kebab_case(name);
    format!(
        "{{\n  \
           \"moduleFederation\": {{\n    \
             \"name\": \"{token}\"\n  \
           }}\n\
         }}\n"
    )
}

/// Render a library public-API barrel that re-exports the sample component.
fn lib_index_source(name: &str) -> String {
    let class_name = format!("{}Component", to_pascal_case(name));
    let token = to_kebab_case(name);
    format!("export {{ {class_name} }} from './lib/{token}.component'\n")
}

/// Plan the set of files for a scaffold request without touching the filesystem.
pub fn plan_generate(options: &GenerateOptions) -> Vec<GeneratedFile> {
    let file_name = to_kebab_case(&options.name);
    let root = options.cwd.join(&file_name);

    match options.kind {
        GenerateKind::App => vec![
            GeneratedFile {
                path: root.join("treaty.config.json"),
                contents: treaty_config_source(&options.name),
            },
            GeneratedFile {
                path: root.join("index.html"),
                contents: index_html_source(&options.name),
            },
            GeneratedFile {
                path: root.join("src").join("main.ts"),
                contents: main_source(&options.name),
            },
            GeneratedFile {
                path: root
                    .join("src")
                    .join("app")
                    .join(format!("{file_name}.component.ts")),
                contents: component_source(&options.name),
            },
        ],
        GenerateKind::Lib => vec![
            GeneratedFile {
                path: root.join("src").join("index.ts"),
                contents: lib_index_source(&options.name),
            },
            GeneratedFile {
                path: root
                    .join("src")
                    .join("lib")
                    .join(format!("{file_name}.component.ts")),
                contents: component_source(&options.name),
            },
        ],
        GenerateKind::Component => vec![GeneratedFile {
            // A bare component is written into the *current* project's src/app.
            path: options
                .cwd
                .join("src")
                .join("app")
                .join(format!("{file_name}.component.ts")),
            contents: component_source(&options.name),
        }],
    }
}

/// Scaffold the requested artifact. Plans via [`plan_generate`], then writes
/// each (creating parent dirs) unless `dry_run`. Existing files are skipped
/// unless `force`, so a generate never clobbers work by accident.
pub fn run_generate(options: &GenerateOptions) -> std::io::Result<GenerateResult> {
    let files = plan_generate(options);
    let mut written = Vec::new();
    let mut skipped = Vec::new();

    if !options.dry_run {
        for file in &files {
            if file.path.exists() && !options.force {
                skipped.push(file.path.clone());
                continue;
            }
            if let Some(parent) = file.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&file.path, &file.contents)?;
            written.push(file.path.clone());
        }
    }

    Ok(GenerateResult {
        kind: options.kind,
        name: options.name.clone(),
        files,
        written,
        skipped,
    })
}

/// Whether `path`'s contents are a selectorless component (used by tests and as
/// a self-check that the scaffold honours the no-boilerplate contract).
pub fn is_selectorless_component(contents: &str) -> bool {
    !contents.contains("selector:") && !contents.contains("standalone:")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(kind: GenerateKind, name: &str) -> GenerateOptions {
        GenerateOptions {
            kind,
            name: name.to_string(),
            cwd: std::env::temp_dir(),
            dry_run: true,
            force: false,
        }
    }

    #[test]
    fn case_helpers() {
        assert_eq!(to_pascal_case("user-profile"), "UserProfile");
        assert_eq!(to_pascal_case("user profile card"), "UserProfileCard");
        assert_eq!(to_kebab_case("UserProfile"), "user-profile");
        assert_eq!(to_kebab_case("userProfileCard"), "user-profile-card");
        assert_eq!(to_kebab_case("  Hello World  "), "hello-world");
    }

    #[test]
    fn generated_component_is_selectorless() {
        let files = plan_generate(&opts(GenerateKind::Component, "user-card"));
        assert_eq!(files.len(), 1);
        let src = &files[0].contents;
        // The whole point: NO selector / standalone / signal boilerplate.
        assert!(!src.contains("selector:"), "must not emit a selector");
        assert!(!src.contains("standalone:"), "must not emit standalone flag");
        assert!(!src.contains("signal("), "must not emit signal() plumbing");
        assert!(is_selectorless_component(src));
        // It is still a real component with the derived class name + a template.
        assert!(src.contains("export class UserCardComponent"));
        assert!(src.contains("@Component({"));
        assert!(src.contains("template:"));
        // Written under src/app.
        assert!(files[0].path.ends_with("src/app/user-card.component.ts"));
    }

    #[test]
    fn app_plan_includes_config_html_main_and_component() {
        let files = plan_generate(&opts(GenerateKind::App, "Shop"));
        let names: Vec<String> = files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"treaty.config.json".to_string()));
        assert!(names.contains(&"index.html".to_string()));
        assert!(names.contains(&"main.ts".to_string()));
        assert!(names.contains(&"shop.component.ts".to_string()));
        // Every generated component is selectorless.
        for f in &files {
            if f.path.to_string_lossy().ends_with("component.ts") {
                assert!(is_selectorless_component(&f.contents));
            }
        }
    }

    #[test]
    fn lib_plan_has_barrel_and_component() {
        let files = plan_generate(&opts(GenerateKind::Lib, "ui-kit"));
        assert!(files.iter().any(|f| f.path.ends_with("src/index.ts")));
        assert!(files
            .iter()
            .any(|f| f.path.ends_with("src/lib/ui-kit.component.ts")));
        let barrel = files
            .iter()
            .find(|f| f.path.ends_with("src/index.ts"))
            .unwrap();
        assert!(barrel.contents.contains("UiKitComponent"));
    }

    #[test]
    fn run_generate_writes_and_skips() {
        let dir = std::env::temp_dir().join(format!("treaty-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let o = GenerateOptions {
            kind: GenerateKind::Component,
            name: "widget".into(),
            cwd: dir.clone(),
            dry_run: false,
            force: false,
        };
        let first = run_generate(&o).unwrap();
        assert_eq!(first.written.len(), 1);
        assert!(first.skipped.is_empty());
        // Second run skips the existing file.
        let second = run_generate(&o).unwrap();
        assert!(second.written.is_empty());
        assert_eq!(second.skipped.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
