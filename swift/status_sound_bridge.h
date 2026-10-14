#ifndef status_sound_bridge_h
#define status_sound_bridge_h

#ifdef __cplusplus
extern "C" {
#endif

/* Load (or replace) the player pool for cue `id` from in-memory audio file
 * bytes (AIFF). Players are prepared (buffers primed) so the first play has
 * no decode/startup delay. Returns 1 on success, 0 on failure. */
int always_sound_load(int id, const unsigned char* data, int len);

/* Start cue `id` on an idle pooled player (a retrigger while one is still
 * sounding uses another player — nothing is dropped). Non-blocking.
 * Returns 1 when a player reported playing, 0 otherwise. */
int always_sound_play(int id);

/* Number of players of cue `id` currently playing (tests/diagnostics). */
int always_sound_playing_count(int id);

/* 1 when the default output device is running anywhere on the system
 * (kAudioDevicePropertyDeviceIsRunningSomewhere), 0 when idle, -1 on error.
 * Used by tests to prove an idle daemon does not hold the output device. */
int always_sound_output_running(void);

#ifdef __cplusplus
}
#endif

#endif /* status_sound_bridge_h */
