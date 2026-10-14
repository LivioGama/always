// Stub for SDKs without AVFoundation: cues are silently unavailable.
@_cdecl("always_sound_load")
public func alwaysSoundLoad(_ id: Int32, _ data: UnsafePointer<UInt8>?, _ len: Int32) -> Int32 { 0 }
@_cdecl("always_sound_play")
public func alwaysSoundPlay(_ id: Int32) -> Int32 { 0 }
@_cdecl("always_sound_playing_count")
public func alwaysSoundPlayingCount(_ id: Int32) -> Int32 { 0 }
@_cdecl("always_sound_output_running")
public func alwaysSoundOutputRunning() -> Int32 { -1 }
