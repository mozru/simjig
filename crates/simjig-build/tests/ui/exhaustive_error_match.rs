// The public error enums are `#[non_exhaustive]`: even a match naming every variant
// needs a wildcard outside the crate, so adding a variant is not a breaking change.
use simjig_build::Error;
use simjig_build::codegen::Error as CodegenError;
use simjig_build::svtypes::error::Error as IrError;

fn build(e: &Error) -> u8 {
    match e {
        Error::Config { .. } => 0,
        Error::MissingEnv { .. } => 1,
        Error::BadEnv { .. } => 2,
        Error::VerilatorNotFound { .. } => 3,
        Error::UnsupportedVerilator { .. } => 4,
        Error::Command { .. } => 5,
        Error::Spawn { .. } => 6,
        Error::Io { .. } => 7,
        Error::Parse { .. } => 8,
        Error::Compile { .. } => 9,
        Error::Ir { .. } => 10,
        Error::Codegen { .. } => 11,
        Error::Emit { .. } => 12,
        Error::Ports(_) => 13,
    }
}

fn ir(e: &IrError) -> u8 {
    match e {
        IrError::Json { .. } => 0,
        IrError::UnsupportedVersion { .. } => 1,
        IrError::Tool { .. } => 2,
        IrError::Io { .. } => 3,
    }
}

fn codegen(e: &CodegenError) -> u8 {
    match e {
        CodegenError::Unsupported { .. } => 0,
        CodegenError::Config { .. } => 1,
    }
}

fn main() {
    let _ = (build, ir, codegen);
}
