fn main() -> core::error::Result { ui::app::run(ui::edit::Edit::new(&ui::text::default_font, ui::edit::Cow::new(std::str::from_utf8(&std::fs::read("examples/text.rs")?)?))) }
