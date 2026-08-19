-- Drive Donkey Kong through real gameplay on the same frame schedule as
-- tools/script/examples/dkong_walk_gameplay.rhai, so the two emulators can be
-- compared on audio the *game* produced rather than on a stimulus someone
-- invented.
--
-- Two runs, selected with DK_WALK: one holds right, one does not. Everything
-- else is identical, so differencing them isolates the footsteps from the music
-- that plays throughout. On the Phosphor side the pre-walk window between the
-- two runs is bit-identical, which is what makes the differencing valid; the
-- same has to hold here or this approach does not work on the MAME side.
--
--   DK_WALK=1 mame dkong -rompath <roms> -cfg_directory /tmp/cleancfg \
--       -nothrottle -seconds_to_run 23 -video none \
--       -autoboot_script tools/sound-reference/drive_dkong_gameplay.lua \
--       -wavwrite /tmp/mame_play_walk.wav
--   (and again without DK_WALK for /tmp/mame_play_still.wav)
--
-- Frame schedule, matching the rhai script exactly.
local COIN_AT = 10
local START_AT = 15
local PLAY_AT = 1000 -- audio capture starts here on the Phosphor side
local SETTLE = 30
local WALK_FRAMES = 250
local TAIL = 15

local HOLD_RIGHT = os.getenv("DK_WALK") ~= nil
local WALK_FROM = PLAY_AT + SETTLE
local WALK_TO = WALK_FROM + WALK_FRAMES
local STOP_AT = WALK_TO + TAIL

local frame = 0
local fields = {}
local resolved = false

-- Find an input field by name across every port. Names differ between MAME
-- versions and drivers, so resolve once and report what is available on a miss
-- rather than silently driving nothing — which is exactly the failure that
-- made an earlier capture look like a silent board.
local function resolve()
  local want = { coin = "Coin 1", start = "1 Player Start", right = "P1 Right" }
  local seen = {}
  for pname, port in pairs(manager.machine.ioport.ports) do
    for fname, field in pairs(port.fields) do
      seen[#seen + 1] = string.format("%s/%s", pname, fname)
      for key, target in pairs(want) do
        if fname == target then fields[key] = field end
      end
    end
  end
  for key, target in pairs(want) do
    if not fields[key] then
      print(string.format("[DRIVE] ERROR: no input field named %q", target))
      print("[DRIVE] available: " .. table.concat(seen, ", "))
      return false
    end
  end
  print(string.format("[DRIVE] resolved inputs; hold_right=%s", tostring(HOLD_RIGHT)))
  return true
end

local function set(key, on)
  if fields[key] then fields[key]:set_value(on and 1 or 0) end
end

local function on_frame()
  if not resolved then
    if not resolve() then
      resolved = true -- do not spam every frame
      return
    end
    resolved = true
  end

  frame = frame + 1

  -- Coin and start are two-frame presses, as in the rhai schedule.
  set("coin", frame >= COIN_AT and frame < COIN_AT + 2)
  set("start", frame >= START_AT and frame < START_AT + 2)

  if HOLD_RIGHT then
    set("right", frame >= WALK_FROM and frame < WALK_TO)
  end

  if frame == PLAY_AT then
    print("[DRIVE] gameplay window opens")
    -- Snapshot at the capture origin, so the two emulators can be confirmed to
    -- be at the same point in the game before any audio is compared. A spectral
    -- comparison of two different musical bars describes the offset, not the
    -- sound path.
    if manager.machine.video then manager.machine.video:snapshot() end
  end
  if frame == STOP_AT then print("[DRIVE] schedule complete") end
end

_G.__drive_sub = nil
if emu.add_machine_frame_notifier then
  _G.__drive_sub = emu.add_machine_frame_notifier(on_frame)
elseif emu.register_frame_done then
  emu.register_frame_done(on_frame)
else
  print("[DRIVE] ERROR: no frame notifier API available")
end
