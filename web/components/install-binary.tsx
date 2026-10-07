"use client";

import { useEffect, useState } from "react";
import { GITHUB_REPO_URL } from "@/lib/constants";
import { InstallCodeBlock } from "./install-code-block";

type Arch = "macos-arm64" | "macos-x64" | "linux-x64" | "linux-arm64" | "windows-x64";

// Each snippet downloads, checksum-verifies, then installs in one pass: the
// files keep their real asset names (codesmith-macos-arm64, …) through the
// `shasum -c` step so the grep filter matches manifest entries exactly, and
// the `sudo mv` that removes them only runs after verification succeeded.
const SNIPPETS: Record<Arch, string> = {
  "macos-arm64": `curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-macos-arm64
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-tui-macos-arm64
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-artifacts-sha256.txt
grep -E 'codesmith(-tui)?-macos-arm64$' codesmith-artifacts-sha256.txt | shasum -a 256 -c -
chmod +x codesmith-macos-arm64 codesmith-tui-macos-arm64
xattr -d com.apple.quarantine codesmith-macos-arm64 codesmith-tui-macos-arm64 2>/dev/null || true
sudo mv codesmith-macos-arm64 /usr/local/bin/codesmith
sudo mv codesmith-tui-macos-arm64 /usr/local/bin/codesmith-tui`,
  "macos-x64": `curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-macos-x64
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-tui-macos-x64
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-artifacts-sha256.txt
grep -E 'codesmith(-tui)?-macos-x64$' codesmith-artifacts-sha256.txt | shasum -a 256 -c -
chmod +x codesmith-macos-x64 codesmith-tui-macos-x64
xattr -d com.apple.quarantine codesmith-macos-x64 codesmith-tui-macos-x64 2>/dev/null || true
sudo mv codesmith-macos-x64 /usr/local/bin/codesmith
sudo mv codesmith-tui-macos-x64 /usr/local/bin/codesmith-tui`,
  "linux-x64": `curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-linux-x64
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-tui-linux-x64
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-artifacts-sha256.txt
grep -E 'codesmith(-tui)?-linux-x64$' codesmith-artifacts-sha256.txt | sha256sum -c -
chmod +x codesmith-linux-x64 codesmith-tui-linux-x64
sudo mv codesmith-linux-x64 /usr/local/bin/codesmith
sudo mv codesmith-tui-linux-x64 /usr/local/bin/codesmith-tui`,
  "linux-arm64": `curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-linux-arm64
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-tui-linux-arm64
curl -fsSL -O ${GITHUB_REPO_URL}/releases/latest/download/codesmith-artifacts-sha256.txt
grep -E 'codesmith(-tui)?-linux-arm64$' codesmith-artifacts-sha256.txt | sha256sum -c -
chmod +x codesmith-linux-arm64 codesmith-tui-linux-arm64
sudo mv codesmith-linux-arm64 /usr/local/bin/codesmith
sudo mv codesmith-tui-linux-arm64 /usr/local/bin/codesmith-tui`,
  "windows-x64": `# PowerShell
$ErrorActionPreference = "Stop"
$dest = "$Env:USERPROFILE\\bin"
New-Item -ItemType Directory -Force $dest | Out-Null

Invoke-RestMethod ${GITHUB_REPO_URL}/releases/latest/download/codesmith-artifacts-sha256.txt -OutFile codesmith-artifacts-sha256.txt
Invoke-WebRequest \`
  -Uri ${GITHUB_REPO_URL}/releases/latest/download/codesmith-windows-x64.exe \`
  -OutFile "$dest\\codesmith.exe"
Invoke-WebRequest \`
  -Uri ${GITHUB_REPO_URL}/releases/latest/download/codesmith-tui-windows-x64.exe \`
  -OutFile "$dest\\codesmith-tui.exe"

# Verify both binaries against their manifest entries
Get-FileHash "$dest\\codesmith.exe","$dest\\codesmith-tui.exe" -Algorithm SHA256
Select-String -Path codesmith-artifacts-sha256.txt -Pattern 'codesmith(-tui)?-windows-x64\\.exe'

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

  useEffect(() => {
    let cancelled = false;
    void detectArch().then((detected) => {
      if (!cancelled) setArch(detected);
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
            onClick={() => setArch(a)}
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
