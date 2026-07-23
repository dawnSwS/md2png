#![windows_subsystem = "windows"]

use arboard::Clipboard;
use msgbox::IconType;
use pulldown_cmark::{CodeBlockKind, Event, Options as MdOptions, Parser as MdParser, Tag, TagEnd};
use std::{
    env,
    error::Error,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use typst::{
    diag::{FileError, FileResult},
    foundations::{Bytes, Datetime},
    syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot},
    text::{Font, FontBook},
    utils::LazyHash,
    Library, LibraryExt, World,
};
use typst_render::RenderOptions;

fn main() {
    if let Err(e) = run() {
        msgbox::create("Md2Png 排版错误", &format!("{}", e), IconType::Error).ok();
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let file_arg = env::args().nth(1).map(PathBuf::from);

    let (content, output_path) = match file_arg {
        Some(path) if path.exists() => {
            let text = fs::read_to_string(&path)?;
            let stem = path
                .file_stem()
                .unwrap_or(OsStr::new("output"))
                .to_string_lossy();
            let parent = path.parent().unwrap_or(Path::new("."));
            (text, get_unique_path(parent, &stem))
        }
        _ => {
            let mut text = String::new();
            for _ in 0..3 {
                if let Ok(mut cb) = Clipboard::new() {
                    if let Ok(t) = cb.get_text() {
                        text = t;
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
            if text.trim().is_empty() {
                return Err("剪贴板为空或无法读取".into());
            }

            let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            (text, get_unique_path(&cwd, "Markdown_NativeRender"))
        }
    };

    let typst_markup = md_to_typst(&content);
    let world = PureWorld::new(typst_markup);

    let compiled = typst::compile::<typst_layout::PagedDocument>(&world);
    let document = match compiled.output {
        Ok(doc) => doc,
        Err(errs) => {
            let msg = errs
                .into_iter()
                .map(|e| e.message.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(format!("原生排版编译失败:\n{}", msg).into());
        }
    };

    if document.pages().is_empty() {
        return Err("生成的物理文档为空白".into());
    }

    let pixmap = typst_render::render(
        &document.pages()[0],
        &RenderOptions {
            pixel_per_pt: 3.0_f64.into(),
            render_bleed: false,
        },
    );

    pixmap
        .save_png(&output_path)
        .map_err(|e| format!("图像落盘失败: {}", e))?;

    Ok(())
}

fn get_unique_path(dir: &Path, stem: &str) -> PathBuf {
    let mut path = dir.join(format!("{}.png", stem));
    let mut i = 1;
    while path.exists() {
        path = dir.join(format!("{} ({}).png", stem, i));
        i += 1;
    }
    path
}

fn md_to_typst(md: &str) -> String {
    let mut opts = MdOptions::empty();
    opts.insert(MdOptions::ENABLE_TABLES);
    opts.insert(MdOptions::ENABLE_STRIKETHROUGH);
    opts.insert(MdOptions::ENABLE_MATH);

    let parser = MdParser::new_ext(md, opts);
    let mut typst = String::new();

    typst.push_str(
        r###"#set page(width: 800pt, height: auto, margin: 40pt, fill: rgb("ffffff"))
#set text(font: ("Microsoft YaHei", "Segoe UI", "SimSun", "Segoe UI Emoji"), size: 24pt)
#set par(leading: 1.5em, justify: true)
#show math.equation: set text(font: ("New Computer Modern Math", "Cambria Math", "Microsoft YaHei", "Segoe UI", "SimSun", "Segoe UI Emoji"))

#show math.equation.where(block: true): it => context {
  let max_w = 720pt
  let unconstrained = block(width: auto, it)
  let m = measure(unconstrained)
  if m.width > max_w {
    let rotated = rotate(-90deg, reflow: true, unconstrained)
    let m_rot = measure(rotated)
    if m_rot.width > max_w {
      let ratio = max_w / m_rot.width
      align(center, scale(x: ratio * 100%, y: ratio * 100%, reflow: true, rotated))
    } else {
      align(center, rotated)
    }
  } else {
    align(center, it)
  }
}

#show raw.where(block: false): box.with(fill: rgb("#f6f8fa"), inset: (x: 4pt, y: 0pt), outset: (y: 3pt), radius: 2pt)

#show raw.where(block: true): it => context {
  let max_w = 720pt
  let unconstrained = block(fill: rgb("#f6f8fa"), inset: 24pt, radius: 8pt, width: auto, text(size: 20pt, it))
  let m = measure(unconstrained)
  if m.width > max_w {
    let ratio = max_w / m.width
    scale(x: ratio * 100%, y: ratio * 100%, origin: left + top, reflow: true, unconstrained)
  } else {
    block(fill: rgb("#f6f8fa"), inset: 24pt, radius: 8pt, width: 100%, text(size: 20pt, it))
  }
}

#show table: it => context {
  let max_w = 720pt
  let m = measure(it)
  if m.width > max_w {
    let ratio = max_w / m.width
    scale(x: ratio * 100%, y: ratio * 100%, origin: left + top, reflow: true, it)
  } else {
    it
  }
}
"###
    );

    let mut in_code_block = false;

    for event in parser {
        match event {
            Event::Text(t) => {
                if in_code_block {
                    typst.push_str(&t);
                } else {
                    typst.push_str(&escape_typst(&t));
                }
            }
            Event::Code(c) => typst.push_str(&format!("`{}`", c.replace('`', "\\`"))),
            Event::SoftBreak | Event::HardBreak => typst.push('\n'),

            Event::InlineMath(m) => {
                let m_typst = tex2typst_rs::tex2typst(&m);
                typst.push_str(&format!(" ${}$ ", m_typst));
            }
            Event::DisplayMath(m) => {
                let m_typst = tex2typst_rs::tex2typst(&m);
                typst.push_str(&format!("\n${}$\n", m_typst));
            }

            Event::Start(Tag::Paragraph) => typst.push_str("\n\n"),
            Event::End(TagEnd::Paragraph) => typst.push_str("\n\n"),
            Event::Start(Tag::Heading { level, .. }) => {
                typst.push_str("\n\n");
                let level_num = match level {
                    pulldown_cmark::HeadingLevel::H1 => 1,
                    pulldown_cmark::HeadingLevel::H2 => 2,
                    pulldown_cmark::HeadingLevel::H3 => 3,
                    pulldown_cmark::HeadingLevel::H4 => 4,
                    pulldown_cmark::HeadingLevel::H5 => 5,
                    pulldown_cmark::HeadingLevel::H6 => 6,
                };
                for _ in 0..level_num {
                    typst.push('=');
                }
                typst.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => typst.push_str("\n\n"),
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang))) => {
                in_code_block = true;
                typst.push_str(&format!("\n\n```{}\n", lang));
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)) => {
                in_code_block = true;
                typst.push_str("\n\n```\n");
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                typst.push_str("\n```\n\n");
            }
            Event::Start(Tag::Strong) => typst.push('*'),
            Event::End(TagEnd::Strong) => typst.push('*'),
            Event::Start(Tag::Emphasis) => typst.push('_'),
            Event::End(TagEnd::Emphasis) => typst.push('_'),
            Event::Start(Tag::Strikethrough) => typst.push_str("#strike["),
            Event::End(TagEnd::Strikethrough) => typst.push(']'),
            Event::Start(Tag::BlockQuote(_)) => {
                typst.push_str("\n#quote(block: true)[\n");
            }
            Event::End(TagEnd::BlockQuote(_)) => typst.push_str("\n]\n"),

            Event::Start(Tag::Table(alignments)) => {
                typst.push_str(&format!("\n#table(\n  columns: {},\n", alignments.len()));
            }
            Event::End(TagEnd::Table) => typst.push_str(")\n"),
            Event::Start(Tag::TableCell) => typst.push_str("  ["),
            Event::End(TagEnd::TableCell) => typst.push_str("],\n"),

            Event::Start(Tag::List(_)) => typst.push('\n'),
            Event::Start(Tag::Item) => typst.push_str("- "),
            Event::End(TagEnd::Item) => typst.push('\n'),

            _ => {}
        }
    }
    typst
}

fn escape_typst(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('$', "\\$")
        .replace('#', "\\#")
        .replace('`', "\\`")
        .replace('<', "\\<")
        .replace('>', "\\>")
        .replace('@', "\\@")
}

struct PureWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    source: Source,
}

impl PureWorld {
    fn new(source_text: String) -> Self {
        let mut fonts = Vec::new();

        for data in typst_assets::fonts() {
            let buffer = Bytes::new(data.to_vec());
            for i in 0..100 {
                if let Some(font) = Font::new(buffer.clone(), i) {
                    fonts.push(font);
                } else {
                    break;
                }
            }
        }

        let font_paths = [
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\msyhbd.ttc",
            r"C:\Windows\Fonts\simsun.ttc",
            r"C:\Windows\Fonts\seguiemj.ttf",
            r"C:\Windows\Fonts\consola.ttf",
            r"C:\Windows\Fonts\cambria.ttc",
            r"C:\Windows\Fonts\cambriam.ttf",
        ];

        for path in font_paths {
            if let Ok(data) = fs::read(path) {
                let buffer = Bytes::new(data);
                for i in 0..100 {
                    if let Some(font) = Font::new(buffer.clone(), i) {
                        fonts.push(font);
                    } else {
                        break;
                    }
                }
            }
        }

        let mut book = FontBook::new();
        for font in &fonts {
            book.push(font.info().clone());
        }

        let virtual_path = VirtualPath::new("main.typ").unwrap();
        let rooted_path = RootedPath::new(VirtualRoot::Project, virtual_path);

        let file_id = rooted_path.intern();

        Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(book),
            fonts,
            source: Source::new(file_id, source_text),
        }
    }
}

impl World for PureWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.source.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.source.id() {
            Ok(self.source.clone())
        } else {
            Err(FileError::NotFound(PathBuf::from(
                id.vpath().get_without_slash(),
            )))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        Err(FileError::NotFound(PathBuf::from(
            id.vpath().get_without_slash(),
        )))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        None
    }
}
