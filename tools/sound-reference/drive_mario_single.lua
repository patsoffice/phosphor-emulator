
-- drive_mario_single.lua
--
-- Capture ONE Mario Bros. discrete effect, driven the way the game drives it,
-- on the same timeline as the matching `sndcmp` scenario in
-- tools/sound-compare/scenarios/mariobros/.
--
-- Select with MARIO_EFFECT: walk1 (Mario), walk2 (Luigi), skid.
--
--   MARIO_EFFECT=skid mame mario -rompath <roms> -nothrottle -seconds_to_run 6 \
--        -video none -sound none -samplerate 192000 \
--        -autoboot_script tools/sound-reference/drive_mario_single.lua \
--        -wavwrite /tmp/mario_skid_ref.wav
--
-- `-samplerate 192000` matters for the same reason it does on the Donkey Kong
-- boards: this game's audio is a netlist, and while the netlist solver has its
-- own internal timestep, the capture's bandwidth is still the output rate. These
-- oscillators run well above the audio band, so a 48 kHz capture aliases them
-- back into it and the comparison measures the alias rather than the circuit.
--
-- THE TWO WALK LINES ARE NOT LATCHES, AND THIS IS THE THING TO GET RIGHT.
--
--   0x7c00  Mario walk  - the WRITE STROBE is the trigger; the data is ignored
--   0x7c80  Luigi walk  - likewise
--   0x7f07  skid        - a real level, and inverted: data 1 drives the line low
--
-- The drawing names the first two `7C00H(WR)` and `7C80H(WR)`, which is exactly
-- what that means: address decode ANDed with WR, straight into a 74123's
-- trigger input. Any write pulses the line low for about 750 ns and it returns
-- high on its own. Writing zero does not "turn it off" - it fires it.
--
-- That is why this driver writes a walk line ONCE and otherwise never touches
-- it. The first version rewrote all three lines every frame, the way a latch
-- driver would, and so triggered every voice sixty times a second in every run
-- including the null one. The captures for Mario, Luigi and null came out
-- byte-identical, which is the null check earning its keep: three identical
-- files are the signature of a driver that is not driving.

local mem
local SPIN = 0x6000 -- main-RAM address holding a JP-to-self

local LINES = { walk1 = 0x7c00, walk2 = 0x7c80, skid = 0x7f07 }
local IS_STROBE = { walk1 = true, walk2 = true, skid = false }

local FRAME_S = 1.0 / 60.0
local PRE_ROLL = 2.0
local HOLD_S = 3 * FRAME_S
local fired = false
local released = false

local effect = os.getenv("MARIO_EFFECT")
if effect == nil or LINES[effect] == nil then
  print("[DRIVER] ERROR: set MARIO_EFFECT to one of: walk1, walk2, skid")
  return
end
local addr = LINES[effect]
print(string.format("[DRIVER] %s: line 0x%04x, assert %.3f s, release %.3f s",
                    effect, addr, PRE_ROLL, PRE_ROLL + HOLD_S))

local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end

  -- Park the main CPU every frame, so nothing but this script drives a sound
  -- line. "The game is quiet here" is a claim needing evidence.
  mem:write_u8(SPIN + 0, 0xc3) -- JP nn
  mem:write_u8(SPIN + 1, SPIN & 0xff)
  mem:write_u8(SPIN + 2, (SPIN >> 8) & 0xff)
  manager.machine.devices[":maincpu"].state["PC"].value = SPIN

  -- Elapsed time, not the attotime's integer seconds field.
  local t = manager.machine.time:as_double()

  local verify = os.getenv("SND_VERIFY")
  if verify == "nudge" then
    t = t - 0.030
  end

  -- Nothing is written to any line unless this run is triggering it. A walk
  -- line cannot be "held low" between events, because touching it at all is an
  -- event; the skid line idles at its released value and the parked CPU never
  -- moves it.
  if verify == "null" then
    return
  end

  if IS_STROBE[effect] then
    -- One write, one footstep. The data byte is discarded by the hardware.
    if not fired and t >= PRE_ROLL then
      mem:write_u8(addr, 1)
      fired = true
    end
  else
    -- The skid line is a level, inverted: 1 pulls it low. Its 74123 fires on
    -- the edge, so the hold length below sets when the line returns rather than
    -- how long the voice lasts.
    if not fired and t >= PRE_ROLL then
      mem:write_u8(addr, 1)
      fired = true
    elseif fired and not released and t >= PRE_ROLL + HOLD_S then
      mem:write_u8(addr, 0)
      released = true
    end
  end
end

_G.__drive_sub = nil
if emu.add_machine_frame_notifier then
  _G.__drive_sub = emu.add_machine_frame_notifier(on_frame)
elseif emu.register_frame_done then
  emu.register_frame_done(on_frame)
else
  print("[DRIVER] ERROR: no frame notifier API available")
end
