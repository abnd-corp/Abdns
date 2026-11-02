# Formule Homebrew : compile abdns depuis les sources d'une version publiée.
# `url` et `sha256` sont mis à jour à chaque version.
class Abdns < Formula
  desc "Encrypted DNS for your computer, with captive portal support"
  homepage "https://github.com/abnd-corp/Abdns"
  url "https://github.com/abnd-corp/Abdns/archive/refs/tags/v0.2.0.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  head "https://github.com/abnd-corp/Abdns.git", branch: "main"

  depends_on "rust" => :build
  depends_on :macos

  def install
    system "cargo", "install", *std_cargo_args
    man1.install "man/abdns.1"
    man8.install "man/abdnsd.8"
    bash_completion.install "completions/abdns.bash" => "abdns"
    zsh_completion.install "completions/_abdns"
    fish_completion.install "completions/abdns.fish"
    pkgshare.install "config.example.toml"
  end

  def post_install
    (etc/"abdns").mkpath
    cp pkgshare/"config.example.toml", etc/"abdns/config.toml" unless (etc/"abdns/config.toml").exist?
  end

  def caveats
    <<~EOS
      Edit your servers in:
        #{etc}/abdns/config.toml
      then start the service (it needs root to listen on port 53 and set the system DNS):
        sudo brew services start abdns
    EOS
  end

  service do
    run [opt_bin/"abdnsd", "--config", etc/"abdns/config.toml"]
    require_root true
    keep_alive true
    log_path "/var/log/abdns/abdns.log"
    error_log_path "/var/log/abdns/abdns.log"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/abdns version")
    system bin/"abdns", "--config", pkgshare/"config.example.toml", "check"
  end
end
