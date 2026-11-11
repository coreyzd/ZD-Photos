import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtMultimedia

// Video playback for the viewer. Loaded on demand, so a computer without Qt Multimedia
// still runs the app; only video playback is missing there.
Item {
    id: pane
    property url source

    function toggle() {
        if (player.playbackState === MediaPlayer.PlayingState) player.pause()
        else player.play()
    }

    function clock(ms) {
        const s = Math.floor(ms / 1000)
        const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, sec = s % 60
        const two = (n) => (n < 10 ? "0" : "") + n
        return h > 0 ? h + ":" + two(m) + ":" + two(sec) : m + ":" + two(sec)
    }

    MediaPlayer {
        id: player
        source: pane.source
        videoOutput: output
        audioOutput: AudioOutput {}
        onSourceChanged: play()
    }

    VideoOutput {
        id: output
        anchors.fill: parent
        fillMode: VideoOutput.PreserveAspectFit
    }

    TapHandler {
        acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchPad
        onTapped: pane.toggle()
    }

    Label {
        visible: player.error !== MediaPlayer.NoError
        anchors.centerIn: parent
        width: Math.min(parent.width - 40, 520)
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        text: "Can't play this video: " + player.errorString
        color: "#e3e3e3"
        font.pixelSize: 14
    }

    // Controls along the bottom.
    Rectangle {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: 64
        gradient: Gradient {
            GradientStop { position: 0; color: "#00000000" }
            GradientStop { position: 1; color: "#b0000000" }
        }
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 16
            anchors.rightMargin: 16
            spacing: 12
            RoundButton {
                text: player.playbackState === MediaPlayer.PlayingState ? "❚❚" : "▶"
                font.pixelSize: 16
                focusPolicy: Qt.NoFocus
                onClicked: pane.toggle()
            }
            Label {
                text: pane.clock(player.position) + " / " + pane.clock(player.duration)
                color: "#e3e3e3"
                font.pixelSize: 13
            }
            Slider {
                id: seek
                Layout.fillWidth: true
                from: 0
                to: Math.max(1, player.duration)
                focusPolicy: Qt.NoFocus
                enabled: player.seekable
                onMoved: player.position = value
                Binding on value {
                    when: !seek.pressed
                    value: player.position
                    restoreMode: Binding.RestoreNone
                }
            }
            ToolButton {
                text: player.audioOutput.muted ? "Unmute" : "Mute"
                font.pixelSize: 12
                focusPolicy: Qt.NoFocus
                onClicked: player.audioOutput.muted = !player.audioOutput.muted
            }
        }
    }
}
