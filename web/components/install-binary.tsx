"use client";

import { useEffect, useRef, useState } from "react";
import { GITHUB_REPO_URL } from "@/lib/constants";
import { InstallCodeBlock } from "./install-code-block";

type Arch = "macos-arm64" | "macos-x64" | "linux-x64" | "linux-arm64" | "windows-x64";

// Each snippet downloads, checksum-verifies, then installs in one pass: the
// files keep their real asset names (codesmith-macos-arm64, …) through the
// `shasum -c` step so the grep filter matches manifest entries exactly, and
// `set -eo pipefail` inside a subshell aborts the paste before the `sudo mv`
// unless verification succeeded (an empty grep match fails the pipeline too
// — which is why pipefail stays despite needing bash/zsh, not POSIX sh).
// Downloads land in a mktemp dir with an EXIT trap: pasting from a
// read-only CWD never reaches the curls, and a verification failure leaves
// nothing behind (the binaries are sudo-mv'd out on success, so the trap
// only ever cleans scratch). The subshell keeps errexit/pipefail from
// leaking into the user's interactive session after the paste finishes.
const unixInstallSnippet = (platform: string, checkCmd: string, isMac: boolean) => `# Requires bash or zsh (pipefail is not POSIX sh)
(
set -eo pipefail
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
cd "$tmpdir"
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-${platform}
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-tui-${platform}
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-artifacts-sha256.txt
grep -E 'codesmith(-tui)?-${platform}$' codesmith-artifacts-sha256.txt | ${checkCmd} -c -
chmod +x codesmith-${platform} codesmith-tui-${platform}
${isMac ? `xattr -d com.apple.quarantine codesmith-${platform} codesmith-tui-${platform} 2>/dev/null || true\n` : ""}sudo mv codesmith-${platform} /usr/local/bin/codesmith
sudo mv codesmith-tui-${platform} /usr/local/bin/codesmith-tui
)`;

const SNIPPETS: Record<Arch, string> = {
  "macos-arm64": unixInstallSnippet("macos-arm64", "shasum -a 256", true),
  "macos-x64": unixInstallSnippet("macos-x64", "shasum -a 256", true),
  "linux-x64": unixInstallSnippet("linux-x64", "sha256sum", false),
  "linux-arm64": unixInstallSnippet("linux-arm64", "sha256sum", false),
  "windows-x64": `# PowerShell
$ErrorActionPreference = "Stop"
$dest = "$Env:USERPROFILE\\bin"
New-Item -ItemType Directory -Force $dest | Out-Null

# Manifest goes next to the binaries in $dest — an elevated PowerShell may
# start in a non-writable CWD (C:\\Windows\\System32), where a relative
# -OutFile would abort the whole install.
$manifest = "$dest\\codesmith-artifacts-sha256.txt"
Invoke-RestMethod ${GITHUB_REPO_URL}/releases/latest/download/codesmith-artifacts-sha256.txt -OutFile $manifest
Invoke-WebRequest \`
  -Uri ${GITHUB_REPO_URL}/releases/latest/download/codesmith-windows-x64.exe \`
  -OutFile "$dest\\codesmith.exe"
Invoke-WebRequest \`
  -Uri ${GITHUB_REPO_URL}/releases/latest/download/codesmith-tui-windows-x64.exe \`
  -OutFile "$dest\\codesmith-tui.exe"

# Verify both binaries against their manifest entries (abort on mismatch)
foreach ($bin in @(@("codesmith.exe", "codesmith-windows-x64.exe"), @("codesmith-tui.exe", "codesmith-tui-windows-x64.exe"))) {
  $actual = (Get-FileHash "$dest\\$($bin[0])" -Algorithm SHA256).Hash.ToLower()
  $line = (Select-String -Path $manifest -Pattern ([regex]::Escape($bin[1]) + '$')).Line
  if (-not $line) { throw "Manifest entry missing for $($bin[1])" }
  $expected = ($line -split '\\s+')[0].ToLower()
  if ($actual -ne $expected) { throw "Checksum mismatch for $($bin[0])" }
}

Remove-Item $manifest
$Env:Path = "$dest;$Env:Path"`,
};

const LABELS: Record<Arch, string> = {
  "macos-arm64": "macOS · Apple Silicon",
  "macos-x64": "macOS · Intel",
  "linux-x64": "Linux · x64",
  "linux-arm64": "Linux · arm64",
  "windows-x64": "Windows · x64",
};

type UserAgentData = {
  getHighEntropyValues?: (hints: string[]) => Promise<{ architecture?: string }>;
};

async function detectArch(): Promise<Arch> {
  if (typeof navigator === "undefined") return "macos-arm64";
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("win")) return "windows-x64";
  if (ua.includes("linux")) {
    if (ua.includes("aarch64") || ua.includes("arm64")) return "linux-arm64";
    return "linux-x64";
  }
  // Chrome-family browsers freeze the UA as "Intel Mac OS X" even on Apple
  // Silicon, so the UA alone cannot distinguish Intel macOS. Consult
  // high-entropy UA-CH data when available; otherwise default to arm64
  // (the common case) — the Intel tab stays one click away.
  const userAgentData = (navigator as Navigator & { userAgentData?: UserAgentData })
    .userAgentData;
  if (userAgentData?.getHighEntropyValues) {
    try {
      const { architecture } = await userAgentData.getHighEntropyValues(["architecture"]);
      if (architecture && /x86|i386/i.test(architecture)) return "macos-x64";
    } catch {
      // UA-CH unavailable — fall through to the default.
    }
  }
  return "macos-arm64";
}

interface Props {
  copyLabel?: string;
  copiedLabel?: string;
  archHint?: string;
}

export function InstallBinary({ copyLabel, copiedLabel, archHint }: Props) {
  const [arch, setArch] = useState<Arch>("macos-arm64");
  // A manual tab click wins over the async detection: without this, a
  // pending getHighEntropyValues resolve (slowest on macOS Chrome, where
  // the archHint encourages clicking Intel while it is in flight) would
  // reset the user's choice back to the detected arch.
  const userTouched = useRef(false);

  useEffect(() => {
    let cancelled = false;
    void detectArch()
      .then((detected) => {
        if (!cancelled && !userTouched.current) setArch(detected);
      })
      .catch(() => {
        // Keep the default arch if detection fails.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div>
      <div className="flex flex-wrap gap-0 mb-3 hairline-t hairline-b hairline-l hairline-r">
        {(Object.keys(SNIPPETS) as Arch[]).map((a, i) => (
          <button
            key={a}
            type="button"
            aria-pressed={arch === a}
            onClick={() => {
              userTouched.current = true;
              setArch(a);
            }}
            className={`px-4 py-1.5 font-mono text-[0.7rem] tracking-wider transition-colors ${
              i > 0 ? "hairline-l" : ""
            } ${arch === a ? "bg-ink text-paper" : "bg-paper hover:bg-paper-deep"}`}
          >
            {LABELS[a]}
          </button>
        ))}
      </div>

      {archHint && <p className="mb-3 text-xs text-ink-mute">{archHint}</p>}

      <InstallCodeBlock cmd={SNIPPETS[arch]} copyLabel={copyLabel} copiedLabel={copiedLabel} />
    </div>
  );
}
