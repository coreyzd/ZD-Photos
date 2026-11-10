use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    let builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("com.zd.photos").qml_files(["qml/Main.qml", "qml/SidebarRow.qml"]),
    )
    .qt_module("Quick")
    .qt_module("QuickControls2")
    .files(["src/library.rs"]);

    // MSVC reads C++ sources in the Windows code page unless told otherwise, which turned the
    // sidebar icons into gibberish on her laptop. (The QML now uses \u escapes as well.)
    // SAFETY: only adds a compiler flag; flag_if_supported is a no-op where it doesn't apply.
    let builder = unsafe {
        builder.cc_builder(|cc| {
            cc.flag_if_supported("/utf-8");
        })
    };
    builder.build();
}
