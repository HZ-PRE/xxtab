#!/usr/bin/env python3
"""Root-only real WireGuard -> xxtab -> official wstunnel -> WireGuard test.

Uses an isolated server network namespace and two reserved test subnets.
Requires iproute2, wireguard-tools, ping, python3 and WSTUNNEL_BIN/XXTAB_BIN.
Does not change the host's default route or DNS.
"""
import os
import pathlib
import signal
import subprocess
import tempfile
import time


def call(*args, input=None, check=True, timeout=20):
    result = subprocess.run(args, input=input, text=True, capture_output=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f"command failed: {args[0]} {args[1:3]}: {result.stderr}")
    return result


def main():
    assert os.geteuid() == 0, "requires root"
    ws = os.environ["WSTUNNEL_BIN"]
    client_exe = os.environ["XXTAB_BIN"]
    ns, host_link, server_link, interface = "xxtab-e2e", "xxtehost", "xxteserver", "xxteclient"
    assert ns not in call("ip", "netns", "list").stdout
    for link in (host_link, server_link, interface):
        assert call("ip", "link", "show", "dev", link, check=False).returncode != 0
    assert not call("ip", "-4", "route", "show", "exact", "172.31.254.2/32").stdout
    created_ns = created_link = False
    processes = []
    with tempfile.TemporaryDirectory(prefix="xxtab-e2e-") as temp:
        directory = pathlib.Path(temp)
        try:
            call("ip", "netns", "add", ns)
            created_ns = True
            call("ip", "link", "add", host_link, "type", "veth", "peer", "name", server_link)
            created_link = True
            call("ip", "link", "set", server_link, "netns", ns)
            call("ip", "address", "add", "172.31.254.1/30", "dev", host_link)
            call("ip", "link", "set", host_link, "up")

            def remote(*args):
                return call("ip", "netns", "exec", ns, *args)

            remote("ip", "link", "set", "lo", "up")
            remote("ip", "address", "add", "172.31.254.2/30", "dev", server_link)
            remote("ip", "link", "set", server_link, "up")
            private_server = call("wg", "genkey").stdout.strip()
            private_client = call("wg", "genkey").stdout.strip()
            public_server = call("wg", "pubkey", input=private_server).stdout.strip()
            public_client = call("wg", "pubkey", input=private_client).stdout.strip()
            server_cfg = directory / "server.conf"
            server_cfg.write_text(f"[Interface]\nPrivateKey = {private_server}\nListenPort = 51820\n[Peer]\nPublicKey = {public_client}\nAllowedIPs = 10.254.252.2/32\n")
            server_cfg.chmod(0o600)
            remote("ip", "link", "add", "wgserver", "type", "wireguard")
            remote("wg", "setconf", "wgserver", str(server_cfg))
            remote("ip", "address", "add", "10.254.252.1/24", "dev", "wgserver")
            remote("ip", "link", "set", "wgserver", "up")
            wg_cfg = directory / "wg.conf"
            wg_cfg.write_text(f"[Interface]\nPrivateKey = {private_client}\nAddress = 10.254.252.2/32\n[Peer]\nPublicKey = {public_server}\nAllowedIPs = 10.254.252.1/32\n")
            wg_cfg.chmod(0o600)
            config = directory / "xxtab.toml"
            server_port = 18088 if os.environ.get('XXTAB_PROXY_RATE') else 18089
            mode = os.environ.get('XXTAB_LATENCY_MODE')
            extra = (f"[transport]\nlatency_mode='{mode}'\ndiagnostics_interval_secs=5\n" if mode else '')
            config.write_text(f"server='ws://172.31.254.2:{server_port}'\npath_prefix='e2e-test-secret'\nremote='127.0.0.1:51820'\nlisten='127.0.0.1:51891'\n[wireguard]\nconfig='wg.conf'\nname='{interface}'\n{extra}")
            env = os.environ.copy()
            env.pop("NO_COLOR", None)
            with (directory / "server.log").open("w") as log:
                server = subprocess.Popen(["ip", "netns", "exec", ns, ws, "server", "--restrict-to", "127.0.0.1:51820", "--restrict-http-upgrade-path-prefix", "e2e-test-secret", "ws://0.0.0.0:18089"], stdout=log, stderr=log, env=env)
            processes.append(server)
            if os.environ.get('XXTAB_PROXY_RATE'):
                proxy = str(pathlib.Path(__file__).with_name('slow_tcp_proxy.py'))
                with (directory / 'proxy.log').open('w') as log:
                    process = subprocess.Popen(['ip', 'netns', 'exec', ns, 'python3', proxy,
                        '18088', '18089', os.environ['XXTAB_PROXY_RATE']], stdout=log, stderr=log)
                processes.append(process)
            time.sleep(0.5)
            with (directory / "client.log").open("w") as log:
                client = subprocess.Popen([client_exe, "run", str(config)], stdout=log, stderr=log, env=env)
            processes.append(client)
            deadline = time.monotonic() + 20
            while "tunnel ready" not in (directory / "client.log").read_text():
                assert client.poll() is None, (directory / "client.log").read_text()
                assert time.monotonic() < deadline, "startup timeout"
                time.sleep(0.1)
            print(call("ping", "-c", "3", "-W", "3", "10.254.252.1").stdout)
            handshakes = call("wg", "show", interface, "latest-handshakes").stdout
            assert int(handshakes.split()[1]) > 0, "no real WireGuard handshake"
            print("real WireGuard handshake and encrypted ICMP roundtrip: PASS")
            if os.environ.get('XXTAB_LAN_BENCH') == '1':
                workload = str(pathlib.Path(__file__).with_name('lan_workload.py'))
                with (directory / 'workload.log').open('w') as log:
                    process = subprocess.Popen(['ip', 'netns', 'exec', ns, 'python3', workload,
                        'server', '10.254.252.1'], stdout=log, stderr=log)
                processes.append(process)
                time.sleep(0.2)
                print(call('python3', workload, 'client', '10.254.252.1', timeout=180).stdout)
            client.send_signal(signal.SIGTERM)
            assert client.wait(timeout=20) == 0, (directory / "client.log").read_text()
            assert call("ip", "link", "show", "dev", interface, check=False).returncode != 0
            assert not call("ip", "-4", "route", "show", "exact", "172.31.254.2/32").stdout
            print("SIGTERM interface and bypass route cleanup: PASS")
            if os.environ.get('XXTAB_LAN_BENCH') == '1':
                for line in (directory / 'client.log').read_text().splitlines():
                    if line.startswith(('transport ', 'stopped:')):
                        print(line)
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=20)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            if created_link:
                call("ip", "link", "delete", host_link, check=False)
            if created_ns:
                call("ip", "netns", "delete", ns, check=False)


if __name__ == "__main__":
    main()
