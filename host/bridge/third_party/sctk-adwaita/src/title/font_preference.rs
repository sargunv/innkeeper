#[derive(Debug)]
pub(crate) struct FontPreference {
    pub name: String,
    pub style: Option<String>,
    pub pt_size: f32,
}
