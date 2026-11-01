use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(
        QmlModule::new("com.zd.photos").qml_files(["qml/Main.qml", "qml/SidebarRow.qml"]),
    )
    .qt_module("Quick")
    .qt_module("QuickControls2")
    .files(["src/library.rs"])
    .build();
}
