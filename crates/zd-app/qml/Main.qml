import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs
import com.zd.photos

// Phase 0 shell: a dark, Google-Photos-like library grid with a resizable
// thumbnail size (Ctrl+wheel or the slider), drive status, and Add folder.
ApplicationWindow {
    id: win
    width: 1280
    height: 820
    visible: true
    title: "ZD-Photos"
    color: "#1f1f1f"

    readonly property color ink: "#e3e3e3"
    readonly property color quiet: "#a8a8a8"
    readonly property color accent: "#a8c7fa"   // Google Photos' light blue on dark

    palette {
        window: "#1f1f1f"
        windowText: "#e3e3e3"
        base: "#2a2a2a"
        text: "#e3e3e3"
        button: "#303030"
        buttonText: "#e3e3e3"
        highlight: "#a8c7fa"
        highlightedText: "#062e6f"
        mid: "#444444"
        dark: "#a8c7fa"
        light: "#3a3a3a"
        toolTipBase: "#303030"
        toolTipText: "#e3e3e3"
    }

    Library {
        id: library
        Component.onCompleted: refresh()
    }

    FolderDialog {
        id: folderDialog
        title: "Choose a folder of photos"
        onAccepted: library.scanFolder(selectedFolder.toString())
    }

    header: ToolBar {
        background: Rectangle { color: "#1f1f1f" }
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 16
            anchors.rightMargin: 16
            spacing: 12

            Label {
                text: "ZD-Photos"
                color: win.ink
                font.pixelSize: 20
                font.weight: Font.Medium
            }
            Label {
                text: library.drives
                color: win.quiet
                font.pixelSize: 13
                elide: Text.ElideRight
                Layout.fillWidth: true
            }
            Label { text: "Size"; color: win.quiet; font.pixelSize: 13 }
            Slider {
                id: sizeSlider
                from: 96; to: 400; value: 180
                Layout.preferredWidth: 160
            }
            Button {
                text: "Refresh drives"
                onClicked: library.refresh()
            }
            Button {
                text: "Add folder…"
                onClicked: folderDialog.open()
            }
        }
    }

    footer: Label {
        text: library.status
        color: win.quiet
        padding: 8
        leftPadding: 16
        font.pixelSize: 13
        background: Rectangle { color: "#191919" }
    }

    GridView {
        id: grid
        anchors.fill: parent
        anchors.margins: 8
        clip: true
        model: library.count
        cellWidth: sizeSlider.value + 4
        cellHeight: sizeSlider.value + 4
        cacheBuffer: 2000
        ScrollBar.vertical: ScrollBar {}

        delegate: Item {
            required property int index
            width: grid.cellWidth
            height: grid.cellHeight

            Rectangle {
                anchors.fill: parent
                anchors.margins: 2
                color: "#2a2a2a"
                clip: true

                Image {
                    anchors.fill: parent
                    source: library.thumbUrl(index)
                    fillMode: Image.PreserveAspectCrop
                    asynchronous: true
                    sourceSize.width: 512
                    sourceSize.height: 512
                    opacity: library.isOffline(index) ? 0.55 : 1.0
                }

                // Videos: play mark + length, bottom-left like Google Photos.
                Rectangle {
                    readonly property string len: library.duration(index)
                    visible: len !== ""
                    anchors.left: parent.left
                    anchors.bottom: parent.bottom
                    anchors.margins: 6
                    width: videoText.implicitWidth + 12
                    height: 20
                    radius: 10
                    color: "#cc000000"
                    Label {
                        id: videoText
                        anchors.centerIn: parent
                        text: "\u25B6 " + parent.len
                        color: win.ink
                        font.pixelSize: 11
                    }
                }

                // Drive unplugged: still browsable, marked like Google Photos' backup badges.
                Rectangle {
                    visible: library.isOffline(index)
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.margins: 6
                    width: offlineText.implicitWidth + 12
                    height: 20
                    radius: 10
                    color: "#cc000000"
                    Label {
                        id: offlineText
                        anchors.centerIn: parent
                        text: "Drive offline"
                        color: win.ink
                        font.pixelSize: 11
                    }
                }

                ToolTip.visible: hover.hovered
                ToolTip.text: library.caption(index)
                HoverHandler { id: hover }
            }
        }

        // Ctrl+wheel resizes thumbnails, like Dolphin.
        WheelHandler {
            acceptedModifiers: Qt.ControlModifier
            onWheel: (event) => {
                sizeSlider.value = Math.max(sizeSlider.from, Math.min(sizeSlider.to,
                    sizeSlider.value + event.angleDelta.y / 6))
            }
        }
    }
}
