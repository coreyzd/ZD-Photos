import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// Full-screen photo/video viewer, over the whole window.
//
// * Shows the grid thumbnail at once, then the sharp 2560 px preview (local cache, the Home
//   Library's shared cache, or made from the original on a worker thread), then, when she
//   zooms past what the preview holds, the original file.
// * Arrows / Home / End / swipe to move; wheel, pinch, double-click or double-tap to zoom;
//   drag to pan. I = info panel, F = full screen, Esc = back to the grid.
FocusScope {
    id: viewer

    required property var library
    signal closed(int index)

    property int index: -1
    property int photoId: -1
    property bool video: false
    property string thumbSource: ""
    property string previewSource: ""
    property string originalSource: ""
    property string videoSource: ""
    property string error: ""
    property var info: ({})
    property bool infoOpen: false
    property bool chrome: true

    // Zoom 1 = whole photo fits. The photo is laid out at fitW*zoom x fitH*zoom at (panX, panY).
    property real zoom: 1
    property real panX: 0
    property real panY: 0
    property real swipe: 0          // finger offset while swiping to the next photo
    readonly property bool zoomed: zoom > 1.01
    readonly property real maxZoom: 8

    property int restoreVisibility: Window.Windowed

    readonly property color ink: "#e3e3e3"
    readonly property color quiet: "#a8a8a8"
    readonly property color accent: "#a8c7fa"

    visible: false

    // ------------------------------------------------------------------ opening and moving

    function openAt(i) {
        visible = true
        show(i)
        forceActiveFocus()
    }

    function close() {
        const w = Window.window
        if (w && w.visibility === Window.FullScreen)
            exitFullScreen()
        visible = false
        videoSource = ""
        closed(index)
    }

    function show(i) {
        if (i < 0 || i >= library.count)
            return
        index = i
        photoId = library.photoId(i)
        video = library.isVideo(i)
        error = ""
        originalSource = ""
        thumbSource = library.thumbUrl(i)
        videoSource = video ? library.videoUrl(i) : ""
        previewSource = library.previewUrl(i)   // empty: on its way, see onPreviewReady
        info = JSON.parse(library.infoJson(i))
        resetZoom()
        // Have the neighbours ready before she gets there.
        if (i + 1 < library.count) library.previewUrl(i + 1)
        if (i > 0) library.previewUrl(i - 1)
    }

    function next() { if (index + 1 < library.count) show(index + 1) }
    function prev() { if (index > 0) show(index - 1) }

    function toggleFullScreen() {
        const w = Window.window
        if (w.visibility === Window.FullScreen) {
            exitFullScreen()
        } else {
            restoreVisibility = w.visibility
            w.showFullScreen()
        }
    }
    function exitFullScreen() {
        const w = Window.window
        if (restoreVisibility === Window.Maximized) w.showMaximized()
        else w.showNormal()
    }

    Connections {
        target: viewer.library
        function onPreviewReady(id, url, err) {
            if (id !== viewer.photoId)
                return   // a neighbour being prefetched
            if (url !== "") viewer.previewSource = url
            else viewer.error = err
        }
        // A scan added photos: positions shift, keep showing the same one.
        function onRevisionChanged() {
            if (!viewer.visible)
                return
            const i = viewer.library.indexOf(viewer.photoId)
            if (i >= 0) viewer.index = i
        }
    }

    // ------------------------------------------------------------------ zoom maths

    function clampPan() {
        const w = area.fitW * zoom, h = area.fitH * zoom
        panX = w <= area.width ? (area.width - w) / 2 : Math.min(0, Math.max(area.width - w, panX))
        panY = h <= area.height ? (area.height - h) / 2 : Math.min(0, Math.max(area.height - h, panY))
    }
    function resetZoom() {
        zoom = 1
        clampPan()
    }
    // Zoom to z keeping the point (px, py) of the view still.
    function zoomAt(px, py, z) {
        z = Math.max(1, Math.min(maxZoom, z))
        const u = (px - panX) / (area.fitW * zoom)
        const v = (py - panY) / (area.fitH * zoom)
        zoom = z
        panX = px - u * area.fitW * z
        panY = py - v * area.fitH * z
        clampPan()
        // Past the preview's own pixels: bring in the original, if it can be shown.
        if (area.fitW * z > preview.implicitWidth * 1.1 && originalSource === "" && !video)
            originalSource = library.originalUrl(index)
    }
    // Double-click / double-tap: fit <-> roughly 1:1 with the original.
    function toggleZoom(px, py) {
        if (zoomed) {
            resetZoom()
        } else {
            const full = (info.width || preview.implicitWidth || 2560) / area.fitW
            zoomAt(px, py, Math.max(2, Math.min(maxZoom, full)))
        }
    }

    Rectangle { anchors.fill: parent; color: "black" }

    // ------------------------------------------------------------------ the photo
    Item {
        id: area
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        anchors.right: viewer.infoOpen ? panel.left : parent.right
        clip: true

        readonly property real aspect: preview.status === Image.Ready ? preview.implicitWidth / preview.implicitHeight
                                     : thumb.status === Image.Ready ? thumb.implicitWidth / thumb.implicitHeight
                                     : 1.5
        readonly property real fitW: Math.min(width, height * aspect)
        readonly property real fitH: fitW / aspect
        onFitWChanged: viewer.clampPan()
        onHeightChanged: viewer.clampPan()

        Item {
            id: stage
            x: viewer.panX + viewer.swipe
            y: viewer.panY
            width: area.fitW * viewer.zoom
            height: area.fitH * viewer.zoom

            Image {
                id: thumb
                anchors.fill: parent
                source: viewer.thumbSource
                smooth: true
                visible: preview.status !== Image.Ready
            }
            Image {
                id: preview
                anchors.fill: parent
                source: viewer.previewSource
                asynchronous: true
                smooth: true
                mipmap: true
            }
            Image {
                id: original
                anchors.fill: parent
                source: viewer.originalSource
                asynchronous: true
                autoTransform: true   // the original carries her camera's rotation flag
                smooth: true
                mipmap: true
                visible: status === Image.Ready
            }
        }

        // Videos play over their poster frame.
        Loader {
            id: videoLoader
            anchors.fill: parent
            active: viewer.visible && viewer.video && viewer.videoSource !== ""
            source: "VideoPane.qml"
            onLoaded: item.source = Qt.binding(() => viewer.videoSource)
        }

        WheelHandler {
            enabled: !viewer.video
            target: null
            onWheel: (event) => viewer.zoomAt(event.x, event.y, viewer.zoom * Math.pow(2, event.angleDelta.y / 480))
        }

        PinchHandler {
            id: pinch
            enabled: !viewer.video
            target: null
            property real startZoom: 1
            property point last
            onActiveChanged: {
                if (active) {
                    startZoom = viewer.zoom
                    last = centroid.position
                }
            }
            onActiveScaleChanged: {
                if (active)
                    viewer.zoomAt(centroid.position.x, centroid.position.y, startZoom * activeScale)
            }
            onCentroidChanged: {
                if (!active)
                    return
                viewer.panX += centroid.position.x - last.x
                viewer.panY += centroid.position.y - last.y
                last = centroid.position
                viewer.clampPan()
            }
        }

        // Drag: pans when zoomed in, otherwise swipes to the next / previous photo.
        DragHandler {
            id: drag
            target: null
            enabled: !pinch.active
            property real lastX: 0
            property real lastY: 0
            onActiveChanged: {
                if (active) {
                    lastX = 0
                    lastY = 0
                    return
                }
                const s = viewer.swipe
                viewer.swipe = 0
                if (!viewer.zoomed && Math.abs(s) > Math.min(120, area.width * 0.12)) {
                    if (s < 0) viewer.next()
                    else viewer.prev()
                }
            }
            onActiveTranslationChanged: {
                const dx = activeTranslation.x - lastX, dy = activeTranslation.y - lastY
                lastX = activeTranslation.x
                lastY = activeTranslation.y
                if (viewer.zoomed) {
                    viewer.panX += dx
                    viewer.panY += dy
                    viewer.clampPan()
                } else {
                    viewer.swipe = activeTranslation.x
                }
            }
        }

        TapHandler {
            enabled: !viewer.video
            onDoubleTapped: (eventPoint) => viewer.toggleZoom(eventPoint.position.x, eventPoint.position.y)
        }
        // On the touchscreen a single tap hides / shows the buttons, like the Photos app.
        TapHandler {
            acceptedDevices: PointerDevice.TouchScreen
            onSingleTapped: viewer.chrome = !viewer.chrome
        }

        BusyIndicator {
            anchors.centerIn: parent
            running: viewer.visible && viewer.previewSource === "" && viewer.error === "" && !viewer.video
            visible: running
        }
        BusyIndicator {
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.margins: 16
            width: 32
            height: 32
            running: original.status === Image.Loading
            visible: running
        }

        // Messages: preview couldn't be made, drive unplugged, no video support...
        Rectangle {
            readonly property string text: viewer.error !== "" && thumb.status !== Image.Ready
                    ? "Can't show this photo: " + viewer.error
                : viewer.video && viewer.videoSource === ""
                    ? "This video's drive isn't plugged in. Plug it in to play it."
                : viewer.video && videoLoader.status === Loader.Error
                    ? "Video playback needs Qt Multimedia, which isn't installed."
                : viewer.error !== "" && viewer.previewSource === ""
                    ? "Showing a small preview: " + viewer.error + "."
                : ""
            visible: text !== ""
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottom: parent.bottom
            anchors.bottomMargin: 24
            width: msg.implicitWidth + 32
            height: 36
            radius: 18
            color: "#cc202020"
            Label {
                id: msg
                anchors.centerIn: parent
                text: parent.text
                color: viewer.ink
                font.pixelSize: 13
            }
        }

        // ---- previous / next arrows
        RoundButton {
            visible: viewer.chrome && viewer.index > 0
            anchors.left: parent.left
            anchors.leftMargin: 16
            anchors.verticalCenter: parent.verticalCenter
            width: 52; height: 52
            text: "‹"
            font.pixelSize: 30
            focusPolicy: Qt.NoFocus
            opacity: hovered ? 1 : 0.7
            onClicked: viewer.prev()
        }
        RoundButton {
            visible: viewer.chrome && viewer.index + 1 < viewer.library.count
            anchors.right: parent.right
            anchors.rightMargin: 16
            anchors.verticalCenter: parent.verticalCenter
            width: 52; height: 52
            text: "›"
            font.pixelSize: 30
            focusPolicy: Qt.NoFocus
            opacity: hovered ? 1 : 0.7
            onClicked: viewer.next()
        }

        // ---- top bar
        Rectangle {
            visible: viewer.chrome
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: parent.top
            height: 72
            gradient: Gradient {
                GradientStop { position: 0; color: "#b0000000" }
                GradientStop { position: 1; color: "#00000000" }
            }
            RowLayout {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.margins: 8
                spacing: 6
                ToolButton {
                    text: "←"
                    font.pixelSize: 22
                    focusPolicy: Qt.NoFocus
                    onClicked: viewer.close()
                    ToolTip.visible: hovered
                    ToolTip.text: "Back to photos (Esc)"
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    Label {
                        text: viewer.info.title || ""
                        color: viewer.ink
                        font.pixelSize: 15
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Label {
                        text: (viewer.info.name ? viewer.info.name + "  ·  " : "")
                              + (viewer.index + 1).toLocaleString(Qt.locale(), "f", 0) + " of "
                              + viewer.library.count.toLocaleString(Qt.locale(), "f", 0)
                        color: viewer.quiet
                        font.pixelSize: 12
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                }
                ToolButton {
                    visible: viewer.zoomed
                    text: Math.round(viewer.zoom * 100) + "%"
                    font.pixelSize: 13
                    focusPolicy: Qt.NoFocus
                    onClicked: viewer.resetZoom()
                    ToolTip.visible: hovered
                    ToolTip.text: "Fit to screen (0)"
                }
                ToolButton {
                    text: "ⓘ"
                    font.pixelSize: 20
                    highlighted: viewer.infoOpen
                    focusPolicy: Qt.NoFocus
                    onClicked: viewer.infoOpen = !viewer.infoOpen
                    ToolTip.visible: hovered
                    ToolTip.text: "Info (I)"
                }
                ToolButton {
                    text: viewer.Window.window && viewer.Window.window.visibility === Window.FullScreen ? "↙" : "↗"
                    font.pixelSize: 20
                    focusPolicy: Qt.NoFocus
                    onClicked: viewer.toggleFullScreen()
                    ToolTip.visible: hovered
                    ToolTip.text: "Full screen (F)"
                }
            }
        }
    }

    // ------------------------------------------------------------------ info panel
    Rectangle {
        id: panel
        visible: viewer.infoOpen
        width: Math.min(340, viewer.width * 0.4)
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        color: "#1f1f1f"

        Flickable {
            anchors.fill: parent
            anchors.margins: 20
            contentHeight: infoColumn.implicitHeight
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            ScrollBar.vertical: ScrollBar {}

            ColumnLayout {
                id: infoColumn
                width: parent.width
                spacing: 14

                RowLayout {
                    Layout.fillWidth: true
                    Label {
                        text: "Info"
                        color: viewer.ink
                        font.pixelSize: 20
                        Layout.fillWidth: true
                    }
                    ToolButton {
                        text: "✕"
                        focusPolicy: Qt.NoFocus
                        onClicked: viewer.infoOpen = false
                    }
                }

                Repeater {
                    model: viewer.info.rows || []
                    delegate: ColumnLayout {
                        required property var modelData
                        Layout.fillWidth: true
                        spacing: 2
                        Label { text: modelData.label; color: viewer.quiet; font.pixelSize: 12 }
                        Label {
                            text: modelData.value
                            color: viewer.ink
                            font.pixelSize: 14
                            wrapMode: Text.Wrap
                            Layout.fillWidth: true
                        }
                    }
                }

                ColumnLayout {
                    visible: (viewer.info.place || "") !== ""
                    Layout.fillWidth: true
                    spacing: 2
                    Label { text: "Location"; color: viewer.quiet; font.pixelSize: 12 }
                    RowLayout {
                        Layout.fillWidth: true
                        Label {
                            text: viewer.info.place || ""
                            color: viewer.ink
                            font.pixelSize: 14
                            Layout.fillWidth: true
                        }
                        Button {
                            text: "Map"
                            focusPolicy: Qt.NoFocus
                            onClicked: Qt.openUrlExternally(viewer.info.map)
                        }
                    }
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2
                    Label { text: "Tags"; color: viewer.quiet; font.pixelSize: 12 }
                    Label {
                        visible: !(viewer.info.tags && viewer.info.tags.length)
                        text: "No tags yet"
                        color: viewer.quiet
                        font.pixelSize: 14
                    }
                    Flow {
                        Layout.fillWidth: true
                        spacing: 6
                        Repeater {
                            model: viewer.info.tags || []
                            delegate: Rectangle {
                                required property string modelData
                                width: tagText.implicitWidth + 20
                                height: 28
                                radius: 14
                                color: "#303030"
                                Label {
                                    id: tagText
                                    anchors.centerIn: parent
                                    text: modelData
                                    color: viewer.ink
                                    font.pixelSize: 12
                                }
                            }
                        }
                    }
                }

                Rectangle { Layout.fillWidth: true; height: 1; color: "#333333" }

                Label {
                    text: viewer.info.backedUp ? "✓ Backed up to the Home Library" : "Not in the Home Library yet"
                    color: viewer.info.backedUp ? viewer.accent : viewer.quiet
                    font.pixelSize: 13
                }

                Label { text: "Files"; color: viewer.quiet; font.pixelSize: 12 }
                Repeater {
                    model: viewer.info.files || []
                    delegate: ColumnLayout {
                        required property var modelData
                        Layout.fillWidth: true
                        spacing: 1
                        RowLayout {
                            Layout.fillWidth: true
                            Label {
                                text: (modelData.home ? "⌂ " : "") + modelData.drive
                                color: viewer.ink
                                font.pixelSize: 14
                                elide: Text.ElideRight
                                Layout.fillWidth: true
                            }
                            Label {
                                text: modelData.state === "online" ? "plugged in"
                                    : modelData.state === "offline" ? "not plugged in" : "missing"
                                color: modelData.state === "online" ? viewer.accent : viewer.quiet
                                font.pixelSize: 12
                            }
                        }
                        Label {
                            text: modelData.path
                            color: viewer.quiet
                            font.pixelSize: 12
                            wrapMode: Text.WrapAnywhere
                            Layout.fillWidth: true
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ keyboard
    Keys.onPressed: (event) => {
        switch (event.key) {
        case Qt.Key_Right: case Qt.Key_PageDown: next(); break
        case Qt.Key_Left: case Qt.Key_PageUp: prev(); break
        case Qt.Key_Home: show(0); break
        case Qt.Key_End: show(library.count - 1); break
        case Qt.Key_Escape: case Qt.Key_Backspace: close(); break
        case Qt.Key_I: infoOpen = !infoOpen; break
        case Qt.Key_F: case Qt.Key_F11: toggleFullScreen(); break
        case Qt.Key_Space:
            if (videoLoader.item) videoLoader.item.toggle()
            else next()
            break
        case Qt.Key_Plus: case Qt.Key_Equal:
            zoomAt(area.width / 2, area.height / 2, zoom * 1.5); break
        case Qt.Key_Minus:
            zoomAt(area.width / 2, area.height / 2, zoom / 1.5); break
        case Qt.Key_0: resetZoom(); break
        default:
            return
        }
        event.accepted = true
    }
}
