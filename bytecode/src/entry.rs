pub const ARGUMENTS_MODULE: &str = "std.process";
pub const ARGUMENTS_TYPE: &str = "Arguments";
/// The host command line supplied to a Foster `main` function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandArguments {
    /// The path or command name used to invoke the Foster program.
    pub executable: String,
    /// Arguments after the executable name.
    pub values: Vec<String>,
}

impl CommandArguments {
    pub fn new(
        executable: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            executable: executable.into(),
            values: values.into_iter().map(Into::into).collect(),
        }
    }
}
