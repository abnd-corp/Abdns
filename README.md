# abdns — encrypted DNS for your computer, with captive portal support

[![CI](https://github.com/abnd-corp/Abdns/actions/workflows/ci.yml/badge.svg?branch=dev)](https://github.com/abnd-corp/Abdns/actions/workflows/ci.yml)

**English** · [Français](README.fr.md)

**abdns is a local DNS-over-HTTPS (DoH) relay for macOS that sends every DNS query to your own resolver first, falls back to privacy-respecting encrypted resolvers, and uses the network's DNS only as a last resort — for example to sign in to a captive portal on train, hotel or airport Wi-Fi. Every name sent in clear is reported to you.**

It is written in Rust, runs as a small system service, and uses [AdGuard dnsproxy](https://github.com/AdguardTeam/dnsproxy) for the encrypted part. Linux and Windows support is planned.

## Why

- **Your resolver, not someone else's.** Point your computer at a self-hosted DoH resolver (Unbound, Knot Resolver, AdGuard Home, Pi-hole behind a DoH front…), even by IP address only.
- **No censorship from public resolvers.** In several countries, large public resolvers now apply court-ordered blocking or leave: in France, Google, Cloudflare, Cisco OpenDNS and DNS4EU have been ordered to block sites, and Quad9 stops serving French users on 24 November 2026.
- **Encrypted fallbacks that stay encrypted.** If your resolver is down, queries go to no-log, unfiltered resolvers outside those jurisdictions, reached by fixed IP address so that no plaintext lookup is ever needed.
- **Captive portals still work.** Native encrypted DNS settings break most captive portals. abdns detects the situation, uses the network DNS just long enough, opens the sign-in page, and switches back to encrypted DNS as soon as it answers.
- **Nothing in clear without you knowing.** While the network DNS is used, every name looked up is shown in a notification and written to a log.

## How it works

```
applications ──► abdns (127.0.0.1:53) ──► dnsproxy ──DoH──► your resolver
                    │                         └──DoH──► Mullvad · DNS.SB · Digitale Gesellschaft
                    ├── local names (.local, .lan, 192.168.x.x reverse…) ──► network DNS, silently
                    └── last resort: network DNS (DHCP), in clear, reported
```

| Situation | What abdns does |
|---|---|
| Your resolver answers | Every query goes to it over DoH |
| Your resolver is down | dnsproxy uses the encrypted fallbacks |
| Every encrypted server fails twice in a row **and** the network DNS answers (captive portal, DoH blocked) | Switches to the network DNS, notifies you, reports each name every 30 s, opens the sign-in page if a portal is detected |
| Encrypted DNS answers again | Switches back and tells you how long it stayed in clear and how many names were sent |
| Still in clear after 15 min | Reminder every 15 min |
| Name of the local network (`nas.lan`, `printer.local`, `router.home.arpa`, DHCP domain, reverse lookup of a private address) | Always sent to the network DNS, never reported |
| DNSSEC failure (SERVFAIL) | Stays an error: never a reason to switch to clear |
| abdns stops or crashes | The system DNS is set back to the network's, so the machine is never left without DNS |

## Installation (macOS)

### Without Nix

1. Install dnsproxy from its [official releases](https://github.com/AdguardTeam/dnsproxy/releases) (`darwin-arm64` for Apple silicon, `darwin-amd64` for Intel):
   ```sh
   curl -fsSLO https://github.com/AdguardTeam/dnsproxy/releases/download/v0.86.0/dnsproxy-darwin-arm64-v0.86.0.tar.gz
   tar -xzf dnsproxy-darwin-arm64-v0.86.0.tar.gz
   sudo install -m 755 darwin-arm64/dnsproxy /usr/local/bin/dnsproxy
   ```
2. Write the configuration, at least your resolver:
   ```sh
   sudo mkdir -p /etc/abdns
   sudo cp config.example.toml /etc/abdns/config.toml
   sudo nano /etc/abdns/config.toml     # set [upstream] url
   ```
3. Build and install the service (Rust toolchain required):
   ```sh
   make release
   sudo make install
   ```

`sudo abdns uninstall` stops the service, removes it and restores the network DNS.

### With nix-darwin

```nix
# flake.nix
inputs.abdns = { url = "github:abnd-corp/Abdns"; inputs.nixpkgs.follows = "nixpkgs"; };
# modules
abdns.darwinModules.default
{
  services.abdns = {
    enable = true;
    upstream.url = "https://203.0.113.5/dns-query";
    networkServices = [ "Wi-Fi" "USB 10/100/1000 LAN" ];
  };
}
```

### A secret DoH path (optional)

If your resolver protects its DoH endpoint with a secret path (`https://203.0.113.5/dns-query/<token>`), keep the token out of the configuration: set `upstream.url` to the prefix ending with `/` and `upstream.token_file` (`tokenFile` with Nix), then:

```sh
printf '%s' 'https://203.0.113.5/dns-query/<token>' | sudo abdns token
```

The token is stored root-only, passed to dnsproxy through a root-only file (never on the command line) and masked in logs.

## Usage

```sh
abdns status                      # mode, network, network DNS, names sent in clear
sudo abdns mode local             # force the network DNS (stubborn captive portal)
sudo abdns mode encrypted         # forbid the network DNS
sudo abdns mode auto              # back to automatic
tail -f /var/log/abdns/abdns.log  # service log
cat /var/log/abdns/clear.log      # every name sent in clear
```

Notifications are shown through `osascript`: allow **Script Editor** in System Settings › Notifications.

## Configuration

See [`config.example.toml`](config.example.toml). Only `[upstream]` is required.

| Key | Default | Meaning |
|---|---|---|
| `language` | `"en"` | Notifications in `en` or `fr` |
| `listen` | `127.0.0.1:53`, `[::1]:53` | Where the system sends its queries |
| `dnsproxy` | found in `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin` | dnsproxy binary |
| `dnsproxy_port` | `5300` | Local port of dnsproxy |
| `upstream.url` | — | Your DoH resolver |
| `upstream.ip` | — | Required if the URL uses a host name |
| `upstream.token_file` | — | Root-only file holding the secret path segment |
| `fallback` | Mullvad, DNS.SB, Digitale Gesellschaft | Encrypted fallbacks (name, url, ip) |
| `network_services` | `["Wi-Fi"]` | macOS network services pointed to abdns |
| `state_dir` | `/var/run/abdns` | Status, forced mode, dnsproxy files |
| `clear_log` | `/var/log/abdns/clear.log` | Names sent in clear |

## Default encrypted fallbacks

| Resolver | Country | Why |
|---|---|---|
| [Mullvad DNS](https://mullvad.net/en/help/dns-over-https-and-dns-over-tls) (unfiltered) | Sweden | No logs, no filtering |
| [DNS.SB](https://dns.sb) | Germany | No logs, fast in Europe |
| [Digitale Gesellschaft](https://www.digitale-gesellschaft.ch/dns/) | Switzerland | Non-profit, no logs |

dnsproxy spreads queries across the fallbacks by speed; they are only used when your resolver does not answer.

## FAQ

**How do I use my own DNS-over-HTTPS server on macOS with a fallback?**
macOS encrypted DNS profiles accept a single server with no fallback. abdns sets the system DNS to a local relay that uses your server first and encrypted fallbacks next.

**Does encrypted DNS work behind a captive portal (SNCF Wi-Fi, hotel, airport)?**
Not with native settings: the portal blocks the encrypted server before you sign in. abdns notices that every encrypted server fails while the network DNS answers, uses the network DNS for the sign-in, opens the portal page, and returns to encrypted DNS right after.

**Which DNS resolvers are not censored in France?**
Your own resolver, or no-log resolvers outside French jurisdiction such as Mullvad, DNS.SB or Digitale Gesellschaft. Google, Cloudflare, Cisco and DNS4EU apply French blocking orders, and Quad9 leaves France on 24 November 2026.

**Are local names like `nas.lan` or reverse lookups of 192.168.x.x sent to the internet?**
No. Names of the local network always go to the network DNS and are never reported, in every mode.

**Does abdns work with a VPN?**
Yes. A VPN such as Proton VPN or WireGuard sets its own DNS while connected; abdns is used again as soon as it disconnects.

**What happens if abdns crashes?**
The system DNS is set back to the network's, and launchd restarts abdns within 30 seconds.

**Do I need Nix?**
No. Nix is one installation method; `make install` works without it.

## Limitations

- macOS only for now; Linux and Windows are planned (`src/platform/`).
- In local mode, answers are not DNSSEC-validated, and a network that deliberately blocks encrypted DNS gets local mode. The notifications make it visible; `abdns mode encrypted` forbids it.
- The network DNS is the one of the DHCP lease (IPv4).
- Browsers or apps with their own secure DNS setting, and iCloud Private Relay, bypass the system DNS. Set Firefox and Chrome secure DNS to "system".

## Compared with

| | abdns | macOS DNS profile | dnscrypt-proxy | dnsproxy alone |
|---|---|---|---|---|
| Your own resolver first, strictly | yes | yes | no (fastest wins) | yes |
| Encrypted fallbacks | yes | no | yes | yes |
| Captive portals | yes, automatic | no | no | no |
| Names sent in clear reported | yes | — | no | no |
| Local names kept local | yes | no | with rules | with rules |

## Development

```sh
make check                      # formatting, Clippy (warnings as errors), tests: what CI runs
make build
```

To try abdns without touching the system DNS, use a `listen` port above 1024 and run `cargo run -- --config my.toml service --no-system`, then query it with `dig @127.0.0.1 -p <port> example.com`. Only `src/platform/` depends on the operating system: Linux and Windows support means implementing its functions there. Dependencies and their changelogs: [`docs/stack.md`](docs/stack.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and the [architecture decision](docs/adr/0001-relais-local-et-dnsproxy.md). Security issues: [SECURITY.md](SECURITY.md).

## License

See [LICENSE](LICENSE).
