"""Smoke test against a running esphome-api server using the official aioesphomeapi client.

Usage: /tmp/esphome-venv/bin/python tests/ha_client_smoke.py [host] [noise_psk_base64]
"""
import asyncio
import sys
import aioesphomeapi
from aioesphomeapi import APIClient, SwitchInfo, NumberInfo, SelectInfo, TextSensorInfo, ButtonInfo

async def main():
    host = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
    psk = sys.argv[2] if len(sys.argv) > 2 else None
    if psk:
        # Wrong key first: HA's config flow relies on this exact error to learn name + MAC.
        wrong = "MDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA="
        bad = APIClient(host, 6053, password=None, noise_psk=wrong, client_info="smoke-test-badkey")
        for attempt in range(40):
            try:
                await bad.connect(login=True)
                raise AssertionError("connected with a wrong key!")
            except aioesphomeapi.InvalidEncryptionKeyAPIError as e:
                print("wrong key rejected as expected; device says name=%s mac=%s" % (e.received_name, e.received_mac))
                assert e.received_name and e.received_mac and len(e.received_mac) == 12, "server hello must carry name and mac"
                if host == "127.0.0.1":
                    assert e.received_name == "relay6-host" and e.received_mac == "020000000001"
                break
            except Exception:
                if attempt == 39:
                    raise
                await asyncio.sleep(0.25)
        # Plaintext client against an encrypted device must be told to use encryption.
        plain = APIClient(host, 6053, password=None, noise_psk=None, client_info="smoke-test-plain")
        try:
            await plain.connect(login=True)
            raise AssertionError("plaintext connected to encrypted device!")
        except aioesphomeapi.RequiresEncryptionAPIError:
            print("plaintext client told to encrypt, as expected")
    cli = APIClient(host, 6053, password=None, noise_psk=psk, client_info="smoke-test")
    for attempt in range(40):
        try:
            await cli.connect(login=True)
            break
        except Exception as e:  # server may still be starting
            if attempt == 39:
                raise
            await asyncio.sleep(0.25)
    info = await cli.device_info()
    print("device_info:", info.name, info.mac_address, info.esphome_version, "encrypted" if psk else "plaintext")
    entities, services = await cli.list_entities_services()
    print(f"entities: {len(entities)}, services: {len(services)}")
    for e in entities:
        print("  ", type(e).__name__, e.object_id, hex(e.key))
    for s in services:
        print("   service", s.name, [(a.name, a.type) for a in s.args])

    states = {}
    got_states = asyncio.Event()
    def on_state(st):
        states[st.key] = st
        if len(states) >= sum(1 for e in entities if not isinstance(e, ButtonInfo)):
            got_states.set()
    cli.subscribe_states(on_state)
    await asyncio.wait_for(got_states.wait(), 5)
    print("initial states received:", len(states))

    async def wait_state(key, pred, what, timeout=3.0):
        for _ in range(int(timeout / 0.05)):
            if key in states and pred(states[key].state):
                return
            await asyncio.sleep(0.05)
        raise AssertionError(f"{what}: state is {states.get(key)}")

    sw = next(e for e in entities if isinstance(e, SwitchInfo) and e.object_id == "relay_1")
    assert states[sw.key].state is False, "relay_1 should start off"
    cli.switch_command(sw.key, True)
    await wait_state(sw.key, lambda s: s is True, "relay_1 did not turn on")
    print("switch relay_1 -> ON confirmed")

    num = next(e for e in entities if isinstance(e, NumberInfo))
    original_max_on = states[num.key].state
    cli.number_command(num.key, 45.0)
    await wait_state(num.key, lambda s: abs(s - 45.0) < 1e-3, "max_on_1 did not update")
    print("number max_on_1 -> 45 confirmed")

    sel = next(e for e in entities if isinstance(e, SelectInfo))
    manual = next(o for o in sel.options if o.lower().startswith("manual"))
    auto = next(o for o in sel.options if o.lower().startswith("auto"))
    cli.select_command(sel.key, manual)
    await wait_state(sel.key, lambda s: s == manual, "mode did not update")
    print("select mode -> manual confirmed")

    svc = next(s for s in services if s.name == "set_schedule")
    await cli.execute_service(svc, {"json": '{"rev":1,"channels":{"1":[{"days":"MTWTFSS","from":0,"to":10}]}}'})
    texts = [e for e in entities if isinstance(e, TextSensorInfo)]
    link = next((e for e in texts if "schedule" in e.object_id), texts[0])
    await wait_state(link.key, lambda s: "schedule" in s or "rev 1" in s, "service did not reach the device")
    print("schedule text sensor now:", states[link.key].state)

    btn = next(e for e in entities if isinstance(e, ButtonInfo) and "off" in e.object_id)
    cli.button_command(btn.key)
    await wait_state(sw.key, lambda s: s is False, "all_off button did not switch relay_1 off")
    print("button all_off confirmed")

    # Leave the device as we found it.
    cli.select_command(sel.key, auto)
    cli.number_command(num.key, original_max_on)
    await asyncio.sleep(0.3)
    await cli.disconnect()
    print("SMOKE TEST PASSED (aioesphomeapi %s)" % aioesphomeapi.__file__.split('/')[-3])

asyncio.run(main())
