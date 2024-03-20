#[derive(Debug, Clone)]
pub struct PluginOptions {
    pub compilation_mode: CompilationMode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompilationMode {
    JIT,
    AOT,
}