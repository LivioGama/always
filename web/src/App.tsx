import { useState, useEffect } from 'react';

/* ------------------------------------------------------------------ */
/*  Icons (inline SVGs, no external deps)                              */
/* ------------------------------------------------------------------ */

function MicIcon({ className = '' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
      <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
      <line x1="12" y1="19" x2="12" y2="23" />
      <line x1="8" y1="23" x2="16" y2="23" />
    </svg>
  );
}

function DesktopIcon({ className = '' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <rect x="2" y="3" width="20" height="14" rx="2" />
      <line x1="8" y1="21" x2="16" y2="21" />
      <line x1="12" y1="17" x2="12" y2="21" />
    </svg>
  );
}

function KeysIcon({ className = '' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <rect x="2" y="4" width="20" height="16" rx="2" />
      <path d="M6 8h.01M10 8h.01M14 8h.01M18 8h.01M6 12h.01M10 12h.01M14 12h.01M18 12h.01M8 16h8" />
    </svg>
  );
}

function ClipboardIcon({ className = '' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" />
      <rect x="8" y="2" width="8" height="4" rx="1" />
    </svg>
  );
}

function GrammarIcon({ className = '' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <path d="M4 7h16M4 12h10M4 17h14" />
      <circle cx="19" cy="17" r="3" />
      <path d="M20 17l1.5 2" />
    </svg>
  );
}

function GlobeIcon({ className = '' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="10" />
      <path d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
    </svg>
  );
}

function ShieldIcon({ className = '' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
    </svg>
  );
}

function DownloadIcon({ className = '' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <polyline points="7 10 12 15 17 10" />
      <line x1="12" y1="15" x2="12" y2="3" />
    </svg>
  );
}


function ChevronDown({ className = '' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round">
      <polyline points="6 9 12 15 18 9" />
    </svg>
  );
}

/* ------------------------------------------------------------------ */
/*  Section helpers                                                    */
/* ------------------------------------------------------------------ */

function Section({ id, className = '', children }: { id?: string; className?: string; children: React.ReactNode }) {
  return (
    <section id={id} className={`py-20 md:py-28 ${className}`}>
      {children}
    </section>
  );
}

function SectionTitle({ children, subtitle }: { children: React.ReactNode; subtitle?: string }) {
  return (
    <div className="text-center mb-16">
      <h2 className="text-3xl md:text-4xl lg:text-5xl font-bold text-white mb-4 tracking-tight">{children}</h2>
      {subtitle && <p className="text-lg text-zinc-400 max-w-2xl mx-auto">{subtitle}</p>}
    </div>
  );
}

function Badge({ children }: { children: React.ReactNode }) {
  return (
    <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium bg-violet-500/10 text-violet-400 border border-violet-500/20">
      {children}
    </span>
  );
}

/* ------------------------------------------------------------------ */
/*  Navigation                                                         */
/* ------------------------------------------------------------------ */

function Navbar() {
  const [scrolled, setScrolled] = useState(false);

  useEffect(() => {
    const handler = () => setScrolled(window.scrollY > 20);
    window.addEventListener('scroll', handler, { passive: true });
    return () => window.removeEventListener('scroll', handler);
  }, []);

  return (
    <nav
      className={`fixed top-0 left-0 right-0 z-50 transition-all duration-300 ${
        scrolled ? 'bg-zinc-950/80 backdrop-blur-xl border-b border-zinc-800/50' : 'bg-transparent'
      }`}
    >
      <div className="max-w-6xl mx-auto px-6 h-16 flex items-center justify-between">
        <a href="#" className="flex items-center gap-2 text-white font-bold text-lg tracking-tight">
          <span className="text-violet-400">●</span> always
        </a>
        <div className="hidden md:flex items-center gap-8">
          <a href="#features" className="text-sm text-zinc-400 hover:text-white transition-colors">Features</a>
          <a href="#how-it-works" className="text-sm text-zinc-400 hover:text-white transition-colors">How It Works</a>
          <a href="#download" className="text-sm text-zinc-400 hover:text-white transition-colors">Download</a>
          <a href="https://github.com/LivioGama/always" target="_blank" rel="noreferrer" className="text-sm text-zinc-400 hover:text-white transition-colors">GitHub</a>
          <a
            href="#download"
            className="px-4 py-2 rounded-lg bg-violet-600 hover:bg-violet-500 text-white text-sm font-medium transition-colors"
          >
            Download
          </a>
        </div>
      </div>
    </nav>
  );
}

/* ------------------------------------------------------------------ */
/*  Hero                                                               */
/* ------------------------------------------------------------------ */

function Hero() {
  return (
    <Section className="min-h-screen flex items-center justify-center relative overflow-hidden">
      {/* Gradient orbs */}
      <div className="absolute top-1/4 -left-32 w-96 h-96 bg-violet-600/20 rounded-full blur-3xl pointer-events-none" />
      <div className="absolute bottom-1/4 -right-32 w-96 h-96 bg-indigo-600/15 rounded-full blur-3xl pointer-events-none" />

      <div className="max-w-4xl mx-auto px-6 text-center relative z-10">
        <div className="flex items-center justify-center gap-2 mb-8">
          <Badge>Open Source</Badge>
          <Badge>macOS · Linux · Windows</Badge>
          <Badge>AGPL-3.0</Badge>
        </div>

        <h1 className="text-5xl md:text-7xl lg:text-8xl font-bold text-white tracking-tight leading-[1.05] mb-6">
          Talk. Your laptop<br />
          <span className="text-transparent bg-clip-text bg-gradient-to-r from-violet-400 via-purple-400 to-indigo-400">
            listens.
          </span>
        </h1>

        <p className="text-lg md:text-xl text-zinc-400 max-w-2xl mx-auto mb-10 leading-relaxed">
          Always is the always-on dictation app. No push-to-talk, no window to open —
          just talk. It listens to your voice and types what you say in any app,
          any text field, anywhere.
        </p>

        <div className="flex flex-col sm:flex-row items-center justify-center gap-4">
          <a
            href="#download"
            className="inline-flex items-center gap-2 px-8 py-4 rounded-xl bg-violet-600 hover:bg-violet-500 text-white font-semibold text-lg transition-all hover:shadow-lg hover:shadow-violet-500/25"
          >
            <DownloadIcon className="w-5 h-5" />
            Download Free
          </a>
          <a
            href="https://github.com/LivioGama/always"
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-2 px-8 py-4 rounded-xl bg-zinc-800/50 hover:bg-zinc-800 text-white font-semibold text-lg transition-all border border-zinc-700/50"
          >
            View on GitHub
          </a>
        </div>

        <div className="mt-16 flex items-center justify-center gap-8 text-zinc-500 text-sm">
          <span className="flex items-center gap-2">
            <ShieldIcon className="w-4 h-4 text-violet-400" /> No telemetry
          </span>
          <span className="flex items-center gap-2">
            <ShieldIcon className="w-4 h-4 text-violet-400" /> No account
          </span>
          <span className="flex items-center gap-2">
            <ShieldIcon className="w-4 h-4 text-violet-400" /> No analytics
          </span>
        </div>

        <div className="mt-12 animate-bounce">
          <ChevronDown className="w-6 h-6 text-zinc-600 mx-auto" />
        </div>
      </div>
    </Section>
  );
}

/* ------------------------------------------------------------------ */
/*  Features                                                           */
/* ------------------------------------------------------------------ */

const features = [
  {
    icon: <MicIcon className="w-7 h-7" />,
    title: 'Voice-Only Listening',
    desc: 'No push-to-talk. The microphone is always live — speech becomes text automatically. Just speak naturally and keep working.',
  },
  {
    icon: <DesktopIcon className="w-7 h-7" />,
    title: 'Cross-Platform',
    desc: 'Works on macOS, Linux, and Windows. Type in any app — Xcode, Slack, Notion, Terminal, your browser — everywhere at once.',
  },
  {
    icon: <KeysIcon className="w-7 h-7" />,
    title: 'Global Hotkeys',
    desc: 'Pause, resume, toggle auto-enter, and correct transcripts without leaving your workflow. All shortcuts are fully customizable.',
  },
  {
    icon: <ClipboardIcon className="w-7 h-7" />,
    title: 'Smart Paste',
    desc: 'Text lands at your cursor with proper spacing and punctuation. Auto-enter submits in forms; hold ⌘ to prevent the send.',
  },
  {
    icon: <GrammarIcon className="w-7 h-7" />,
    title: 'Grammar Correction',
    desc: 'An AI-powered layer corrects your transcription in real-time — fixing grammar, handling jargon, and learning from your edits.',
  },
  {
    icon: <GlobeIcon className="w-7 h-7" />,
    title: 'Local & Cloud STT',
    desc: 'Use Groq\'s blazing-fast cloud API or run whisper, Parakeet, and other models entirely on-device. Switch or hybrid — your choice.',
  },
  {
    icon: <ShieldIcon className="w-7 h-7" />,
    title: 'No Volume Drop',
    desc: 'Smart microphone conflict handling auto-pauses when another app needs the mic, auto-resumes when it\'s free. No fighting with Zoom.',
  },
];

function Features() {
  return (
    <Section id="features" className="bg-zinc-950">
      <div className="max-w-6xl mx-auto px-6">
        <SectionTitle subtitle="Everything you need for hands-free dictation — nothing you don't.">
          Powerful features,<br />zero distractions
        </SectionTitle>

        <div className="grid md:grid-cols-2 lg:grid-cols-3 gap-6">
          {features.map((f, i) => (
            <div
              key={i}
              className="group p-6 rounded-2xl bg-zinc-900/50 border border-zinc-800/50 hover:border-violet-500/30 hover:bg-zinc-900/80 transition-all duration-300"
            >
              <div className="w-12 h-12 rounded-xl bg-violet-500/10 flex items-center justify-center text-violet-400 mb-4 group-hover:bg-violet-500/20 transition-colors">
                {f.icon}
              </div>
              <h3 className="text-lg font-semibold text-white mb-2">{f.title}</h3>
              <p className="text-sm text-zinc-400 leading-relaxed">{f.desc}</p>
            </div>
          ))}
        </div>
      </div>
    </Section>
  );
}

/* ------------------------------------------------------------------ */
/*  How It Works                                                       */
/* ------------------------------------------------------------------ */

const steps = [
  {
    num: '01',
    title: 'Install',
    desc: 'Download the app for your platform — macOS DMG, Linux DEB, or Windows MSI. One click and it\'s in your menu bar.',
    icon: <DownloadIcon className="w-8 h-8" />,
  },
  {
    num: '02',
    title: 'Enroll Your Voice',
    desc: 'Record three short samples. Always builds a personal voiceprint so it only listens to you — ignoring backgrounds, calls, and media.',
    icon: <MicIcon className="w-8 h-8" />,
  },
  {
    num: '03',
    title: 'Start Speaking',
    desc: 'That\'s it. Just talk. Your words appear at your cursor in any app, any text field — automatically, instantly, everywhere.',
    icon: <KeysIcon className="w-8 h-8" />,
  },
];

function HowItWorks() {
  return (
    <Section id="how-it-works" className="bg-zinc-950">
      <div className="max-w-5xl mx-auto px-6">
        <SectionTitle subtitle="Three steps. Less than five minutes. Your hands are free from then on.">
          How it works
        </SectionTitle>

        <div className="grid md:grid-cols-3 gap-8">
          {steps.map((s, i) => (
            <div key={i} className="relative text-center">
              {/* Connector line */}
              {i < steps.length - 1 && (
                <div className="hidden md:block absolute top-10 left-[60%] w-[80%] h-px bg-gradient-to-r from-violet-500/40 to-transparent" />
              )}
              <div className="w-20 h-20 rounded-2xl bg-gradient-to-br from-violet-600/20 to-indigo-600/10 border border-violet-500/20 flex items-center justify-center text-violet-400 mx-auto mb-6">
                {s.icon}
              </div>
              <div className="text-xs font-mono text-violet-500 mb-2">{s.num}</div>
              <h3 className="text-xl font-semibold text-white mb-3">{s.title}</h3>
              <p className="text-sm text-zinc-400 leading-relaxed max-w-xs mx-auto">{s.desc}</p>
            </div>
          ))}
        </div>
      </div>
    </Section>
  );
}

/* ------------------------------------------------------------------ */
/*  Download                                                           */
/* ------------------------------------------------------------------ */

function Download() {
  const [platform, setPlatform] = useState<'mac' | 'linux' | 'win'>('mac');

  const platforms: Record<string, { label: string; arch: string; color: string }> = {
    mac: { label: 'macOS', arch: 'Apple Silicon (ARM64)', color: 'from-violet-500 to-indigo-600' },
    linux: { label: 'Linux', arch: 'x86_64', color: 'from-emerald-500 to-teal-600' },
    win: { label: 'Windows', arch: 'x64', color: 'from-blue-500 to-cyan-600' },
  };

  return (
    <Section id="download" className="bg-zinc-950">
      <div className="max-w-4xl mx-auto px-6">
        <SectionTitle subtitle="Free and open source. Choose your platform below.">
          Download Always
        </SectionTitle>

        {/* Platform selector */}
        <div className="flex items-center justify-center gap-2 mb-10">
          {Object.entries(platforms).map(([key, p]) => (
            <button
              key={key}
              onClick={() => setPlatform(key as 'mac' | 'linux' | 'win')}
              className={`px-6 py-3 rounded-xl text-sm font-medium transition-all ${
                platform === key
                  ? 'bg-zinc-100 text-zinc-900 shadow-lg shadow-zinc-500/10'
                  : 'bg-zinc-800/50 text-zinc-400 hover:text-white hover:bg-zinc-800'
              }`}
            >
              {p.label}
            </button>
          ))}
        </div>

        {/* Platform-specific card */}
        <div className="bg-zinc-900/60 border border-zinc-800/50 rounded-2xl p-8 md:p-12 text-center">
          <div className={`inline-block mb-6 px-4 py-1.5 rounded-full text-sm font-medium bg-gradient-to-r ${platforms[platform].color} bg-clip-text text-transparent`}>
            {platforms[platform].label} — {platforms[platform].arch}
          </div>

          <div className="flex flex-col items-center gap-4">
            <a
              href="https://github.com/LivioGama/always/releases/latest"
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-2 px-10 py-4 rounded-xl bg-violet-600 hover:bg-violet-500 text-white font-semibold text-lg transition-all hover:shadow-lg hover:shadow-violet-500/25"
            >
              <DownloadIcon className="w-5 h-5" />
              Download Latest Release
            </a>
            <span className="text-xs text-zinc-500">
              Version 0.0.1 · Released June 2026 · Built with Tauri v2
            </span>
          </div>

          {platform === 'mac' && (
            <div className="mt-8 text-left max-w-md mx-auto space-y-3 text-sm text-zinc-400">
              <p>📦 Format: DMG (signed & notarized)</p>
              <p>🍎 Requires: macOS 14 (Sonoma) or later</p>
              <p>🔑 After install: System Settings → Privacy & Security → Allow Always to run</p>
            </div>
          )}
          {platform === 'linux' && (
            <div className="mt-8 text-left max-w-md mx-auto space-y-3 text-sm text-zinc-400">
              <p>📦 Format: DEB package (x86_64)</p>
              <p>🐧 Requires: Ubuntu 22.04+ / Debian 12+ / Fedora 38+</p>
              <p>🔑 After install: Enable microphone & accessibility permissions</p>
            </div>
          )}
          {platform === 'win' && (
            <div className="mt-8 text-left max-w-md mx-auto space-y-3 text-sm text-zinc-400">
              <p>📦 Format: MSI installer (x64)</p>
              <p>🪟 Requires: Windows 10 1909+ or Windows 11</p>
              <p>🔑 After install: Run installer as Administrator for full microphone access</p>
            </div>
          )}
        </div>

        {/* System requirements */}
        <div className="mt-12 grid sm:grid-cols-3 gap-6 text-center">
          <div className="p-5 rounded-xl bg-zinc-900/30 border border-zinc-800/30">
            <div className="text-2xl font-bold text-white mb-1">80 MB</div>
            <div className="text-xs text-zinc-500">Disk Space</div>
          </div>
          <div className="p-5 rounded-xl bg-zinc-900/30 border border-zinc-800/30">
            <div className="text-2xl font-bold text-white mb-1">4 GB</div>
            <div className="text-xs text-zinc-500">RAM (8 GB recommended)</div>
          </div>
          <div className="p-5 rounded-xl bg-zinc-900/30 border border-zinc-800/30">
            <div className="text-2xl font-bold text-white mb-1">∞</div>
            <div className="text-xs text-zinc-500">Usage — forever free</div>
          </div>
        </div>
      </div>
    </Section>
  );
}

/* ------------------------------------------------------------------ */
/*  Screenshots Placeholder                                            */
/* ------------------------------------------------------------------ */

function Screenshots() {
  return (
    <Section className="bg-zinc-950">
      <div className="max-w-5xl mx-auto px-6">
        <SectionTitle subtitle="A peek inside the app.">
          Screenshots
        </SectionTitle>

        <div className="grid md:grid-cols-2 gap-6">
          {[
            { label: 'Menu Bar Status', img: 'docs/always-settings.png' },
            { label: 'Dictation Overlay', img: 'docs/always_overlay.png' },
            { label: 'Settings Panel', img: 'docs/always_settings.png' },
            { label: 'Always Icon', img: 'docs/always-mark.webp' },
          ].map((s, i) => (
            <div key={i} className="group rounded-2xl overflow-hidden bg-zinc-900/50 border border-zinc-800/50 hover:border-violet-500/20 transition-all">
              <div className="aspect-video bg-zinc-900 flex items-center justify-center">
                {/* Placeholder image area */}
                <div className="text-center">
                  <div className="w-16 h-16 rounded-2xl bg-zinc-800 flex items-center justify-center mx-auto mb-3 group-hover:bg-zinc-700 transition-colors">
                    <svg className="w-8 h-8 text-zinc-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5}>
                      <rect x="3" y="3" width="18" height="18" rx="2" />
                      <circle cx="8.5" cy="8.5" r="1.5" />
                      <path d="M21 15l-5-5L5 21" />
                    </svg>
                  </div>
                  <p className="text-sm text-zinc-500">{s.label}</p>
                </div>
              </div>
            </div>
          ))}
        </div>
      </div>
    </Section>
  );
}

/* ------------------------------------------------------------------ */
/*  Footer                                                             */
/* ------------------------------------------------------------------ */

function Footer() {
  return (
    <footer className="bg-zinc-950 border-t border-zinc-800/50 py-12">
      <div className="max-w-6xl mx-auto px-6">
        <div className="grid md:grid-cols-4 gap-8 mb-12">
          {/* Brand */}
          <div className="md:col-span-2">
            <a href="#" className="flex items-center gap-2 text-white font-bold text-lg tracking-tight mb-3">
              <span className="text-violet-400">●</span> always
            </a>
            <p className="text-sm text-zinc-500 max-w-sm leading-relaxed">
              The always-on dictation app. No push-to-talk, no window to open — just talk.
              Built by humans who got tired of typing.
            </p>
          </div>

          {/* Links */}
          <div>
            <h4 className="text-sm font-semibold text-white mb-3">Product</h4>
            <ul className="space-y-2 text-sm text-zinc-500">
              <li><a href="#features" className="hover:text-white transition-colors">Features</a></li>
              <li><a href="#download" className="hover:text-white transition-colors">Download</a></li>
              <li><a href="#how-it-works" className="hover:text-white transition-colors">How It Works</a></li>
            </ul>
          </div>

          <div>
            <h4 className="text-sm font-semibold text-white mb-3">Resources</h4>
            <ul className="space-y-2 text-sm text-zinc-500">
              <li><a href="https://github.com/LivioGama/always" target="_blank" rel="noreferrer" className="hover:text-white transition-colors">GitHub</a></li>
              <li><a href="https://github.com/LivioGama/always/releases" target="_blank" rel="noreferrer" className="hover:text-white transition-colors">Releases</a></li>
              <li><a href="https://github.com/LivioGama/also/issues" target="_blank" rel="noreferrer" className="hover:text-white transition-colors">Issues</a></li>
              <li><a href="docs/ARCHITECTURE.md" className="hover:text-white transition-colors">Architecture</a></li>
            </ul>
          </div>
        </div>

        <div className="border-t border-zinc-800/50 pt-8 flex flex-col md:flex-row items-center justify-between gap-4">
          <p className="text-xs text-zinc-600">
            © {new Date().getFullYear()} Always. Licensed under AGPL-3.0.
          </p>
          <div className="flex items-center gap-4 text-xs text-zinc-600">
            <a href="https://github.com/LivioGama/also" target="_blank" rel="noreferrer" className="hover:text-zinc-400 transition-colors">GitHub</a>
            <span>·</span>
            <span>AGPL-3.0</span>
            <span>·</span>
            <span>No telemetry · No analytics · No account</span>
          </div>
        </div>
      </div>
    </footer>
  );
}

/* ------------------------------------------------------------------ */
/*  Root App                                                           */
/* ------------------------------------------------------------------ */

export default function App() {
  return (
    <div className="min-h-screen bg-zinc-950 text-white antialiased selection:bg-violet-500/30">
      <Navbar />
      <main>
        <Hero />
        <Features />
        <HowItWorks />
        <Download />
        <Screenshots />
      </main>
      <Footer />
    </div>
  );
}