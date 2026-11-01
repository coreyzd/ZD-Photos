import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs
import com.zd.photos

// Library window: drives sidebar on the left, photo grid on the right, scan progress at the
// bottom. Dark, Google-Photos-like colours; Dolphin-like resizable thumbnails.
ApplicationWindow {
    id: win
    width: 1360
    height: 860
    visible: true
    title: "ZD-Photos"
    color: "#1f1f1f"

    readonly property color ink: "#e3e3e3"
    readonly property color quiet: "#a8a8a8"
    readonly property color accent: "#a8c7fa"   // Google Photos' light blue on dark
    readonly property color panel: "#191919"

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
        Component.onCompleted: {
            refresh()
            // Developer convenience: `zd-photos --scan <folder>` starts a scan on launch.
            const args = Qt.application.arguments
            const i = args.indexOf("--scan")
            if (i >= 0 && i + 1 < args.length)
                scanFolder(Qt.resolvedUrl("file://" + args[i + 1]).toString())
        }
        // Rebuild the grid but keep her place, so photos appearing during a scan don't
        // throw the view back to the top.
        onRevisionChanged: {
            const y = grid.contentY
            grid.model = 0
            grid.model = library.count
            grid.contentY = Math.min(y, Math.max(0, grid.contentHeight - grid.height))
        }
    }

    FolderDialog {
        id: folderDialog
        title: "Choose a folder of photos"
        onAccepted: library.scanFolder(selectedFolder.toString())
    }

    // ---- rename a drive
    Dialog {
        id: renameDialog
        property int volumeId: -1
        title: "Rename drive"
        modal: true
        anchors.centerIn: parent
        standardButtons: Dialog.Ok | Dialog.Cancel
        TextField {
            id: renameField
            width: 300
            selectByMouse: true
            onAccepted: renameDialog.accept()
        }
        onAccepted: library.renameVolume(volumeId, renameField.text)
        function openFor(id, current) {
            volumeId = id
            renameField.text = current
            open()
            renameField.selectAll()
            renameField.forceActiveFocus()
        }
    }

    // ---- scanning a cloud drive downloads everything: confirm first
    Dialog {
        id: cloudDialog
        property int volumeId: -1
        title: "Scan a cloud drive?"
        modal: true
        anchors.centerIn: parent
        standardButtons: Dialog.Yes | Dialog.No
        Label {
            width: 380
            wrapMode: Text.WordWrap
            text: "This drive keeps its files online. Scanning it makes your computer download every photo on it, which can take a long time and use a lot of disk space."
        }
        onAccepted: library.scanVolume(volumeId)
    }

    Menu {
        id: driveMenu
        property int volumeId: -1
        property string label: ""
        property string kind: ""
        property bool online: false
        MenuItem {
            text: "Rename…"
            onTriggered: renameDialog.openFor(driveMenu.volumeId, driveMenu.label)
        }
        MenuItem {
            text: "Scan this drive"
            enabled: driveMenu.online && !library.scanning
            onTriggered: driveMenu.kind === "cloud"
                ? (cloudDialog.volumeId = driveMenu.volumeId, cloudDialog.open())
                : library.scanVolume(driveMenu.volumeId)
        }
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
                Layout.preferredWidth: 212
            }
            Label {
                text: library.viewTitle
                color: win.ink
                font.pixelSize: 16
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
                enabled: !library.scanning
                onClicked: library.refresh()
            }
            Button {
                text: "Add folder…"
                enabled: !library.scanning
                onClicked: folderDialog.open()
            }
        }
    }

    footer: Rectangle {
        color: win.panel
        height: 40
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 16
            anchors.rightMargin: 16
            spacing: 12
            Label {
                visible: !library.scanning
                text: library.status
                color: win.quiet
                font.pixelSize: 13
                elide: Text.ElideRight
                Layout.fillWidth: true
            }
            Label {
                visible: library.scanning
                text: library.progressPhase === "finding"
                    ? "Looking for photos… " + library.progressDone.toLocaleString(Qt.locale(), "f", 0) + " found"
                    : library.progressPhase === "checking"
                    ? "Checking photos already in your library: " + library.progressDone.toLocaleString(Qt.locale(), "f", 0)
                      + " of " + library.progressTotal.toLocaleString(Qt.locale(), "f", 0)
                    : "Adding photos: " + library.progressDone.toLocaleString(Qt.locale(), "f", 0)
                      + " of " + library.progressTotal.toLocaleString(Qt.locale(), "f", 0)
                color: win.ink
                font.pixelSize: 13
            }
            ProgressBar {
                visible: library.scanning
                Layout.fillWidth: true
                indeterminate: library.progressPhase === "finding" || library.progressTotal === 0
                from: 0
                to: Math.max(1, library.progressTotal)
                value: library.progressDone
            }
            Label {
                visible: library.scanning
                text: library.progressFile
                color: win.quiet
                font.pixelSize: 12
                elide: Text.ElideMiddle
                Layout.preferredWidth: 220
            }
            Button {
                visible: library.scanning
                text: "Stop"
                onClicked: library.cancelScan()
            }
        }
    }

    RowLayout {
        anchors.fill: parent
        spacing: 0

        // ---------------------------------------------------------------- sidebar
        Rectangle {
            Layout.preferredWidth: 240
            Layout.fillHeight: true
            color: win.panel

            ColumnLayout {
                anchors.fill: parent
                anchors.topMargin: 8
                spacing: 0

                SidebarRow {
                    Layout.fillWidth: true
                    Layout.leftMargin: 8
                    Layout.rightMargin: 8
                    glyph: "▦"
                    label: "All photos"
                    selected: library.selectedVolume === -1
                    online: true
                    onClicked: library.selectVolume(-1)
                }

                Label {
                    text: "DRIVES"
                    color: win.quiet
                    font.pixelSize: 11
                    font.letterSpacing: 1
                    Layout.topMargin: 16
                    Layout.leftMargin: 20
                    Layout.bottomMargin: 4
                }

                ListView {
                    id: driveList
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    model: library.volumeCount
                    Connections {
                        target: library
                        function onRevisionChanged() { driveList.model = 0; driveList.model = library.volumeCount }
                    }
                    delegate: SidebarRow {
                        required property int index
                        readonly property int vid: library.volumeId(index)
                        readonly property string kind: library.volumeKind(index)
                        x: 8
                        width: driveList.width - 16
                        glyph: kind === "home" ? "⌂" : kind === "cloud" ? "☁" : kind === "card" ? "▣"
                             : kind === "network" ? "⇄" : kind === "internal" ? "▤" : "▭"
                        label: library.volumeLabel(index)
                        detail: library.volumePhotos(index) > 0 ? library.volumePhotos(index).toLocaleString(Qt.locale(), "f", 0) : ""
                        online: library.volumeOnline(index)
                        selected: library.selectedVolume === vid
                        onClicked: library.selectVolume(vid)
                        onRightClicked: {
                            driveMenu.volumeId = vid
                            driveMenu.label = label
                            driveMenu.kind = kind
                            driveMenu.online = online
                            driveMenu.popup()
                        }
                        onDoubleClicked: renameDialog.openFor(vid, label)
                    }
                }
            }
        }

        // ---------------------------------------------------------------- grid
        GridView {
            id: grid
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.margins: 8
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

                    // No thumbnail yet (or the file couldn't be read): show its name instead of a blank tile.
                    Column {
                        readonly property string name: library.placeholder(index)
                        visible: name !== ""
                        anchors.centerIn: parent
                        width: parent.width - 16
                        spacing: 6
                        Label {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: "\u25A1"
                            color: win.quiet
                            font.pixelSize: 28
                        }
                        Label {
                            width: parent.width
                            horizontalAlignment: Text.AlignHCenter
                            text: parent.name
                            color: win.quiet
                            font.pixelSize: 11
                            elide: Text.ElideMiddle
                        }
                    }

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
                            text: "▶ " + parent.len
                            color: win.ink
                            font.pixelSize: 11
                        }
                    }

                    // Drive unplugged: still browsable from the cache.
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
}
