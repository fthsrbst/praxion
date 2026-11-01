# Azalea headless protocol body survey (pinned commit 8041a14)

Pinned checkout: `spikes/azalea/` at commit `8041a14` (branch `26.3`), with local patches `spikes/patches/azalea-8041a14-26.3.patch`. Workspace version `0.16.0+mc26.3`, `PROTOCOL_VERSION = 777` (`azalea-protocol/src/packets/mod.rs:14`). All claims below cite that checkout. Items I could not verify are marked **unverified**.

## 1. Login without the `azalea` bevy app

Yes. `azalea-protocol` alone can drive `Handshake -> Login -> Configuration -> Play`; no bevy app is required. `azalea-client` is only a consumer of these types (`azalea-client/src/plugins/join.rs:161-186`, `azalea-client/src/plugins/login.rs`, `azalea-client/src/plugins/packet/config/mod.rs`). The connection state machine and compression/encryption helpers are in `azalea-protocol/src/connect.rs`.

Minimal call sequence (offline mode; online mode adds the `Hello`/`ServerboundKey`/encryption branch):

1. Resolve the address: `azalea_protocol::resolver::resolve_address` (referenced in the doc example at `azalea-protocol/src/connect.rs:70-95`; **unverified** exact signature).
2. `let mut conn = Connection::new(&resolved_address).await?;` (`azalea-protocol/src/connect.rs:349`).
3. `conn.write(ServerboundIntention { protocol_version: PROTOCOL_VERSION, hostname, port, intention: ClientIntention::Login })` (`azalea-client/src/plugins/join.rs:170-176`).
4. `let mut conn = conn.login();` (`azalea-protocol/src/connect.rs:407`).
5. `conn.write(ServerboundHello { name, profile_id })` (`azalea-client/src/plugins/join.rs:236-239`).
6. Read loop over `ClientboundLoginPacket` (`azalea-protocol/src/connect.rs:70-95`):
   - `Hello(p)` (online only): `azalea_crypto::encrypt` -> `ServerboundKey`, then `conn.set_encryption_key(key)` (`azalea-protocol/src/connect.rs:440` / `566`; `azalea-client/src/plugins/login.rs` `auth_with_account`).
   - `LoginCompression(p)`: `conn.set_compression_threshold(p.compression_threshold)` (`azalea-protocol/src/connect.rs:426` / `552`).
   - `CustomQuery(p)`: `ServerboundCustomQueryAnswer { transaction_id, data: None }` (`azalea-client/src/plugins/login.rs` `reply_to_custom_queries`).
   - `CookieRequest(p)`: `ServerboundCookieResponse { key, payload: None }` (`azalea-protocol/src/connect.rs:92-94`).
   - `LoginFinished(p)`: `let mut conn = conn.config();` (`azalea-protocol/src/connect.rs:451` / `596`), store `game_profile`.
   - `LoginDisconnect(p)`: abort.
7. Configuration state (`Connection<ClientboundConfigPacket, ServerboundConfigPacket>`):
   - `ClientboundSelectKnownPacks`: reply `ServerboundSelectKnownPacks { known_packs: vec![] }` (`azalea-client/src/plugins/packet/config/mod.rs` `select_known_packs`).
   - `ClientboundRegistryData`: append entries to the world registries (`azalea-client/src/plugins/packet/config/mod.rs` `registry_data`).
   - `ClientboundKeepAlive`: reply `ServerboundKeepAlive { id }` (`azalea-client/src/plugins/packet/config/mod.rs` `keep_alive`).
   - `ClientboundPing`: reply `ServerboundPong` (`azalea-client/src/plugins/pong.rs`).
   - `ClientboundFinishConfiguration`: set state to `Game`, write `ServerboundFinishConfiguration`, then `conn.game()` (`azalea-client/src/plugins/packet/config/mod.rs` `finish_configuration`; `azalea-protocol/src/connect.rs:606` / `616`).
8. Play state: reply to `ClientboundKeepAlive` with `ServerboundKeepAlive { id }` (`azalea-client/src/plugins/packet/game/mod.rs:925-936`).

Compression threshold and encryption key are held in `RawReadConnection`/`RawWriteConnection` and applied by `read_raw_packet`/`encode_to_network_packet` (`azalea-protocol/src/connect.rs:43-57`, `azalea-client/src/plugins/connection.rs:300-318`). In offline mode the server never sends `Hello`, so no encryption is enabled; **unverified** whether a vanilla offline server sends `LoginCompression` (Azalea handles it if it does).

## 2. Physics

`azalea-physics` is **not** a plain function library. It is a bevy plugin: `PhysicsPlugin::build` registers `fluids::update_in_water_state_and_do_fluid_pushing`, `update_old_position`, `fluids::update_swimming`, `ai_step`, `travel::travel`, `apply_effects_from_blocks` into the `GameTick` schedule (`azalea-physics/src/lib.rs:39-52`). The registered systems are bevy system functions taking `Query<..., (With<LocalEntity>, With<HasClientLoaded>)>` plus `Res<Worlds>`, `AabbQuery`, `CollidableEntityQuery` (`azalea-physics/src/lib.rs:68-83`, `azalea-physics/src/travel.rs:27-45`, `azalea-physics/src/fluids.rs:17-22`).

State types required:
- `Physics` (fields: `velocity`, `old_position`, `x/y/z_acceleration`, `on_ground`, `last_on_ground`, `no_jump_delay`, `bounding_box`, `has_impulse`, `horizontal/vertical_collision`, `water_fluid_height`, `lava_fluid_height`, `was_touching_water`, `fall_distance`, `remaining_fire_ticks`) (`azalea-entity/src/lib.rs:261-343`).
- `Position`, `LookDirection`, `Attributes`, `EntityDimensions`, `Sprinting`, `Jumping`, `OnClimbable`, `Pose`, `PlayerAbilities`, `ClientMovementState` (`azalea-physics/src/client_movement.rs:10-34`).

Movement tick order inside one tick (from `azalea-client/src/plugins/movement.rs:57-70`): `tick_controls` -> `local_player_ai_step` -> `update_pose` (all `PhysicsSystems`, before `ai_step` and fluids) -> `ai_step` -> fluids -> `travel` -> `send_position`. `local_player_ai_step` sets `x_acceleration`/`z_acceleration` from input and toggles sprinting (`azalea-client/src/plugins/movement.rs:292-420`).

Already-pure helpers that can be reused without lifting:
- `jump_from_ground` (`azalea-physics/src/lib.rs:350-383`), `jump_in_liquid` (private, `lib.rs:139-141`), `go_down_in_water` (private, `lib.rs:137`).
- `travel::fluid_jump_threshold` (`azalea-physics/src/travel.rs:352`), `no_collision` (`travel.rs:276`), `get_block_pos_below_that_affects_movement` (`lib.rs:395`).
- `move_colliding`, `get_block_collisions`, `get_block_and_liquid_collisions` (`collision/mod.rs`, `collision/world_collisions.rs`), which take `&World` rather than bevy state.
- `get_fluid_flow` (`fluids.rs:183`).

Jumping / sprinting / sneaking / water / lava / fall distance:
- Jumping: `ai_step` consumes `Jumping(true)` and calls `jump_from_ground` or `jump_in_liquid` (`lib.rs:95-135`); `no_jump_delay` gating at `lib.rs:129`.
- Sprinting: `Sprinting` component flows into `travel` (`travel.rs:69`) and `get_friction_influenced_speed` (`lib.rs:486-500`); start/stop toggled in `azalea-client/src/plugins/movement.rs` `local_player_ai_step` (`movement.rs:380-410`) and `set_sprinting` (`movement.rs:526-540`).
- Sneaking: `ClientMovementState.trying_to_crouch` triggers `go_down_in_water` (`lib.rs:137-139`); crouching pose lowers climb speed in `handle_on_climbable` (`lib.rs:460-484`); sneaking speed multiplier in `modify_input` (`movement.rs:373-390`).
- Water/lava: `was_touching_water`, `water_fluid_height`, `lava_fluid_height` (`Physics` fields); `is_in_water`/`is_in_lava` methods (`azalea-entity/src/lib.rs`); fluid pushing in `fluids.rs`.
- Fall distance: `Physics.fall_distance` and `reset_fall_distance` (`azalea-entity/src/lib.rs:309`, `368`). **Unverified**: fall-damage application is marked `TODO` in `move_colliding` (`collision/mod.rs:160-170` area) and `apply_effects_from_blocks` only checks inside blocks; I did not find fall-damage code in these files.

Lifting cost: the systems themselves are ~70 lines (`ai_step`), ~73 (`travel`), ~43 (fluid state), ~32 (`apply_effects_from_blocks`). The glue to construct `MoveCtx`, `CanPlayerFitCtx`, and the `AabbQuery`/`CollidableEntityQuery` for a single local player is the bulk; estimate **300-600 lines** if we keep `Physics`/`Position` unchanged and call the existing pure helpers, or **~0 lines** if we instead run `azalea-physics` systems inside a minimal bevy `World` (still a bevy dependency, but no `azalea-client`). Full extraction of the collision module is not needed. **Unverified**: whether bevy's `Query` types can be satisfied with empty entity-collision sets for a single player; azalea-client always runs them over the full ECS world.

## 3. Movement packets

Per tick, in `GameTick`, in this order (`azalea-client/src/plugins/movement.rs:57-70`, `azalea-client/src/plugins/tick_end.rs:18-24`):

1. `send_player_input_packet` (`movement.rs:198-232`): sends `ServerboundPlayerInput { forward, backward, left, right, jump, shift, sprint }` **only when the input differs from `LastSentInput`**.
2. `send_sprinting_if_needed` (`movement.rs:233-260`): sends `ServerboundPlayerCommand { action: StartSprinting | StopSprinting }` **only when the sprinting flag changed**.
3. `send_position` (`movement.rs:89-197`): sends at most one of:
   - `ServerboundMovePlayerPosRot { pos, look_direction, flags }` if position delta is large enough (or remainder >= 20) and rotation changed;
   - `ServerboundMovePlayerPos { pos, flags }` if only position;
   - `ServerboundMovePlayerRot { look_direction, flags }` if only rotation;
   - `ServerboundMovePlayerStatusOnly { flags }` if neither moved but on_ground changed;
   - nothing otherwise. `MoveFlags { on_ground, horizontal_collision }`.
4. `game_tick_packet` (`tick_end.rs:29-37`): sends `ServerboundClientTickEnd` every tick, scheduled after `send_position` and mining.

Keep-alive is event-driven, not per tick: `ClientboundKeepAlive` -> `ServerboundKeepAlive { id }` (`azalea-client/src/plugins/packet/game/mod.rs:925-936`). `ClientboundPing` -> `ServerboundPong` (`azalea-client/src/plugins/pong.rs`).

## 4. Digging / placing / attacking / using items / inventory clicks

All of these live in `azalea-client` and are ECS-bound (bevy systems/observers/messages). Packet sequences:

- Digging (survival):
  - `StartMiningBlockEvent` -> `handle_start_mining_block_event` inserts `MiningQueued` (`mining.rs:158-195`).
  - `handle_mining_queued` (`mining.rs:216-388`) sends `ServerboundPlayerAction { StartDestroyBlock, pos, direction, seq }` where `seq = BlockStatePredictionHandler::start_predicting()` (`interact/mod.rs:98-101`), plus `SwingArmEvent` -> `ServerboundPunch` (`interact/mod.rs` `handle_swing_arm_trigger`). If the block instamines, `FinishMiningBlockEvent` is triggered immediately.
  - Each continued tick: `continue_mining_block` (`mining.rs:579-708`) on survival increments progress; when `progress >= 1` sends `ServerboundPlayerAction { StopDestroyBlock, pos, direction, seq }` and triggers `FinishMiningBlockEvent`; creative sends `StartDestroyBlock` again plus `SwingArmEvent`.
  - Abort: `StopMiningBlockEvent` -> `handle_stop_mining_block_event` sends `ServerboundPlayerAction { AbortDestroyBlock, pos, direction, seq: 0 }` (`mining.rs:540-568`).
  - Sequence acks: `ClientboundBlockChangedAck { seq }` -> `BlockStatePredictionHandler::end_prediction_up_to(seq, &world)` (`packet/game/mod.rs:1108-1115`, `interact/mod.rs:142-162`); server block updates reconcile via `update_known_server_state` (`interact/mod.rs:124-138`, `block_update.rs`).
  - Finish: `handle_finish_mining_block_observer` sets the block to air/fluid locally and calls `retain_known_server_state` (`mining.rs:470-533`).
- Placing / right-click block:
  - `StartUseItemEvent` -> `handle_start_use_item_queued` (`interact/mod.rs:211-293`) sends `ServerboundUseItemOn { hand, block_hit, seq }` for a block hit, or `ServerboundUseItem { hand, seq, x_rot, y_rot }` on miss, with `seq = start_predicting()`. Swing is a TODO in the code (not sent on place) (`interact/mod.rs:245-250`).
- Attacking entities:
  - `AttackEvent` inserts `AttackQueued`; `handle_attack_queued` (`attack.rs:49-100`) sends `ServerboundAttack { entity_id }` + `SwingArmEvent` -> `ServerboundPunch`, then multiplies velocity by 0.6 and clears sprinting.
- Using items (eat/drink):
  - Same path as above (`ServerboundUseItem`), repeated while the item is in use. **Unverified**: Azalea's code sends one `StartUseItem` per right-click; holding to eat would need repeated triggers. I did not find a "use for N ticks" loop in these files.
- Inventory clicks:
  - `ContainerClickEvent` -> `handle_container_click_event` (`inventory/mod.rs:167-225`) simulates the click, computes changed slots, sends `ServerboundContainerClick { container_id, state_id, slot_num, button_num, click_type, changed_slots, carried_item }`.
  - Close: `CloseContainerEvent` -> `ServerboundContainerClose { container_id }` (`inventory/mod.rs:91-127`).
  - Hotbar change: `ensure_has_sent_carried_item` sends `ServerboundSetCarriedItem { slot }` when `selected_hotbar_slot` changed (`inventory/mod.rs:287-309`).

## 5. Pathfinder

- A* core: `azalea/src/pathfinder/astar/mod.rs` `a_star` generic function (`Path`, `Edge`, `Movement`, `Node`, `PathfinderTimeout`), plus `heap.rs` and `nodemap.rs`. ~460 lines total (322 + 108 + 30). No bevy imports in `astar/mod.rs` beyond tracing/time; purely generic over `P: Eq + Hash + Copy + Debug`. Lifts trivially. Risk #14 is resolved for the A* core itself.
- Cost constants: `azalea/src/pathfinder/costs.rs` (`WALK_ONE_BLOCK_COST`, `SPRINT_ONE_BLOCK_COST`, `JUMP_PENALTY`, `BLOCK_BREAK_ADDITIONAL_PENALTY`, `FALL_N_BLOCKS_COST`, etc.). ~98 lines, pure. Lifts trivially.
- Moves: `azalea/src/pathfinder/moves/mod.rs` + `basic.rs`, `parkour.rs`, `uncommon.rs`, plus `ExecuteCtx`/`MovesCtx`/`IsReachedCtx`. Heavily coupled to bevy ECS (`Commands`, `MessageWriter`, `Entity`), `azalea-client` events (`StartWalkEvent`, `StartSprintEvent`, `StartMiningBlockEvent`, `SetSelectedHotbarSlotEvent`), and `azalea-world`. Per the agreed decision, only the A* core and cost constants are reused; place/bridge/pillar moves are ours.
- World cache: `azalea/src/pathfinder/world.rs` `CachedWorld` wraps `Arc<RwLock<World>>` with a chunk/section cache. Reusable if we keep `azalea-world`; not required for (b) if we reuse our decoder world and add a small cache later.
- Lifting effort: ~460 lines (A* + heap + nodemap) + ~98 lines (costs) = ~560 lines, none of it ECS-bound. Moves are out of scope per decision.

## 6. Licence

- Workspace license: `license = "MIT"` in `[workspace.package]` (`spikes/azalea/Cargo.toml`, top-level). Every crate sets `license.workspace = true` (verified for all 16 crates: `azalea`, `azalea-auth`, `azalea-block`, `azalea-brigadier`, `azalea-buf`, `azalea-chat`, `azalea-client`, `azalea-core`, `azalea-crypto`, `azalea-entity`, `azalea-inventory`, `azalea-language`, `azalea-physics`, `azalea-protocol`, `azalea-registry`, `azalea-world`).
- Single `LICENSE.md` at the workspace root: MIT, copyright 2022 mat.
- Third-party dependency licenses (bevy, tokio, serde, etc.) were **not** surveyed; **unverified**. The version manifest should record them before release (architecture.md §4 "Version manifest").

## 7. Recommendation

Recommendation: **(b) build our own thin client on `azalea-protocol` + lifted `azalea-physics` functions**, using `azalea-client` only as a reference for packet sequences, not as a dependency.

Reasoning:
- `azalea-protocol` already implements the full `Handshake -> Login -> Configuration -> Play` transition, compression and encryption helpers, and keep-alive is a one-line reply. There is no bevy dependency in the transport layer, so it composes directly with our decoder's single world model and the perception API.
- The A* core and cost constants lift cleanly (~560 lines, no ECS), satisfying the agreed "only A* core + costs" decision and closing risk #14 for the planner.
- The action packet sequences in `azalea-client` are short and readable (mining ~737 lines, interact ~468, inventory ~309, attack ~158); porting the *packet calls* rather than the *ECS machinery* keeps the ControlFrame -> packets mapping explicit and testable for receipts/attribution (§5.3, E9).
- It avoids a second world model and a bevy app inside the body, which would conflict with the decoder owning the raw store and the "body owns one tick source" attribution rule.

Risks:
- Physics parity: a lifted or manually-driven physics step must match the server's movement or movement receipts stay `unconfirmed` (§5.3). Mitigate with a differential test against `azalea-client` in its simulation mode (`azalea-client/tests/simulation/`).
- Packet ordering and prediction sequence numbers must be reproduced exactly (block-change acks, `retain_known_server_state`); missing one breaks rollback. Port from the cited modules, do not improvise.
- Eating/using items for more than one tick is **unverified** in the azalea code I read; may need repeated `StartUseItem` triggers.
- Fall damage appears unimplemented in the pinned physics (`TODO` in `move_colliding`); **unverified** against vanilla. If needed for Demo-0, check before relying on it.
- If lifting physics stalls, fall back to option (c): run `azalea-physics` systems inside a minimal bevy `World` while keeping `azalea-protocol` for networking (no `azalea-client`). Do not take option (a) unless schedule pressure forces it: it would import a full bevy app and azalea's world model, duplicating the decoder and coupling the body to azalea's release cadence.

FINAL_REPORT: docs/research/demo0-azalea-headless.md written. Recommendation (b): thin client on azalea-protocol + lifted physics, reusing only A* core/costs per decision; azalea-client as packet-sequence reference. Risks: physics parity, exact prediction seqs, eating loop and fall damage unverified; fallback (c) minimal bevy world if lifting stalls.