import SwiftUI

/// Settings → My Voice: guided speaker enrollment + the "only listen
/// to my voice" gate toggle. All recording happens in the daemon (it
/// owns the microphone); this panel drives it over UDS through
/// `VoiceEnrollmentClient` and renders live progress.
struct MyVoicePanel: View {
    @ObservedObject var stateMonitor: StateMonitor
    @ObservedObject private var client: VoiceEnrollmentClient = .shared
    @State private var confirmingDelete = false
    /// Mic whose voiceprint the user asked to delete (confirmation pending).
    @State private var micPendingDelete: VoiceMicData? = nil

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header
                gateToggleCard
                micCard
                Text("Record the three short samples below — about 5 seconds of speech each — so Always learns what YOUR voice sounds like across the ways you actually talk. Each microphone keeps its own voiceprint: Always switches to the right one when you change mic.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                VStack(spacing: 8) {
                    ForEach(VoiceEnrollmentClient.Step.allCases) { step in
                        stepRow(step)
                    }
                }
                if let error = client.lastError {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundColor(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if !client.mics.isEmpty {
                    savedMicsList
                } else if !client.recordedSteps.isEmpty {
                    deleteRow
                }
                if !stateMonitor.isDaemonConnected {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Waiting for the Always daemon…")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                }
            }
            .padding(20)
        }
        .onAppear { client.requestStatus() }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("My Voice").font(.headline)
            Text("Teach Always your voice. Once enrolled and enabled, dictation responds ONLY to you — movies, music, videos, and other people around you are ignored, even while media keeps playing at full volume.")
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var gateToggleCard: some View {
        HStack(spacing: 10) {
            Image(systemName: client.isEnabled && client.isEnrolled
                ? "person.wave.2.fill" : "person.wave.2")
                .font(.title3)
                .foregroundColor(client.isEnabled && client.isEnrolled ? .accentColor : .secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text("Only listen to my voice")
                    .font(.subheadline.bold())
                Text(gateStatusText)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            Toggle("", isOn: Binding(
                get: { client.isEnabled },
                set: { client.setEnabled($0) }
            ))
            .toggleStyle(.switch)
            .labelsHidden()
            .disabled(!client.isEnrolled || !stateMonitor.isDaemonConnected)
        }
        .padding(10)
        .background(
            (client.isEnabled && client.isEnrolled ? Color.accentColor : Color.secondary)
                .opacity(0.08)
        )
        .cornerRadius(6)
    }

    private var gateStatusText: String {
        if !client.isEnrolled {
            return "Record all three samples below to unlock."
        }
        return client.isEnabled
            ? "Active — Always ignores every voice except yours."
            : "Enrolled but off — Always currently listens to any voice."
    }

    @ViewBuilder
    private func stepRow(_ step: VoiceEnrollmentClient.Step) -> some View {
        let recorded = client.recordedSteps.contains(step.rawValue)
        let isRecording = client.recordingStep == step
        let otherRecording = client.recordingStep != nil && !isRecording

        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Image(systemName: recorded ? "checkmark.circle.fill" : "circle")
                    .font(.title3)
                    .foregroundColor(recorded ? .green : .secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(step.title).font(.subheadline.bold())
                    Text(step.prompt)
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                Spacer()
                if isRecording {
                    Button("Cancel") { client.cancelRecording() }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                } else {
                    Button(recorded ? "Re-record" : "Record") { client.record(step) }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(!stateMonitor.isDaemonConnected || otherRecording)
                }
            }
            if isRecording {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 8) {
                        Image(systemName: "mic.fill")
                            .font(.caption)
                            .foregroundColor(.red)
                        LevelMeter(level: client.level)
                    }
                    ProgressView(value: client.progress)
                        .controlSize(.small)
                    Text(client.isPreparing
                        ? "Preparing voice model… (first run can take a minute)"
                        : client.progress > 0
                        ? "Keep speaking… \(Int(client.progress * 100))%"
                        : "Listening — start speaking now.")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
                .padding(.leading, 28)
            }
        }
        .padding(10)
        .background(
            (isRecording ? Color.accentColor : Color.secondary).opacity(0.08)
        )
        .cornerRadius(6)
    }

    /// Which mic the samples below are for, and whether it has a voiceprint.
    @ViewBuilder
    private var micCard: some View {
        let missing = client.isEnrolled && !client.micEnrolled
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: missing ? "exclamationmark.triangle.fill" : "mic.fill")
                .font(.title3)
                .foregroundColor(missing ? .orange : .secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text("Microphone: \(client.currentMic ?? "unknown")")
                    .font(.subheadline.bold())
                Text(missing
                    ? "No voiceprint for this mic yet. Your voice sounds different through it, so Always may ignore what you say. Record the three samples below while using this mic."
                    : client.micEnrolled && client.isEnrolled
                    ? "This mic has its own voiceprint."
                    : "Samples you record now are saved for this mic.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
        }
        .padding(10)
        .background((missing ? Color.orange : Color.secondary).opacity(0.08))
        .cornerRadius(6)
    }

    /// Every saved voiceprint, each deletable on its own.
    private var savedMicsList: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Saved voiceprints").font(.subheadline.bold())
            ForEach(client.mics) { mic in
                HStack(spacing: 8) {
                    Image(systemName: mic.complete ? "checkmark.circle.fill" : "circle.dashed")
                        .foregroundColor(mic.complete ? .green : .secondary)
                    Text(mic.name).font(.caption)
                    if !mic.complete {
                        Text("incomplete").font(.caption2).foregroundColor(.secondary)
                    }
                    Spacer()
                    Button(role: .destructive) {
                        micPendingDelete = mic
                    } label: {
                        Image(systemName: "trash")
                    }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .disabled(!stateMonitor.isDaemonConnected)
                    .help("Delete the voiceprint for \(mic.name)")
                }
            }
            deleteRow
        }
        .padding(10)
        .background(Color.secondary.opacity(0.08))
        .cornerRadius(6)
        .confirmationDialog(
            "Delete the voiceprint for \(micPendingDelete?.name ?? "this mic")?",
            isPresented: Binding(
                get: { micPendingDelete != nil },
                set: { if !$0 { micPendingDelete = nil } }
            ),
            presenting: micPendingDelete
        ) { mic in
            Button("Delete", role: .destructive) { client.deleteProfile(forMic: mic.uid) }
            Button("Cancel", role: .cancel) {}
        } message: { mic in
            Text("Always forgets how your voice sounds through \(mic.name). Your other microphones keep theirs.")
        }
    }

    private var deleteRow: some View {
        HStack {
            Spacer()
            Button(role: .destructive) {
                confirmingDelete = true
            } label: {
                Label(client.mics.count > 1 ? "Delete all voiceprints" : "Delete voiceprint", systemImage: "trash")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(!stateMonitor.isDaemonConnected)
            .confirmationDialog(
                client.mics.count > 1
                    ? "Delete the voiceprints of all \(client.mics.count) microphones?"
                    : "Delete your enrolled voiceprint?",
                isPresented: $confirmingDelete
            ) {
                Button("Delete", role: .destructive) { client.deleteProfile() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Always will listen to any voice again until you re-enroll.")
            }
        }
    }
}

/// Simple horizontal mic-level meter. The daemon reports normalized
/// RMS where normal speech sits around 0.002–0.05, so the display
/// rescales with a square-root curve to keep quiet speech visible.
private struct LevelMeter: View {
    let level: Double

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule()
                    .fill(Color.secondary.opacity(0.15))
                Capsule()
                    .fill(Color.accentColor)
                    .frame(width: geo.size.width * displayFraction)
                    .animation(.linear(duration: 0.1), value: displayFraction)
            }
        }
        .frame(height: 6)
    }

    private var displayFraction: CGFloat {
        let normalized = min(1.0, level / 0.05)
        return CGFloat(normalized.squareRoot())
    }
}
