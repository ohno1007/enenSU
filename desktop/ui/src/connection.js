import { svg } from 'lit';

// The wire and both screens change when the selected ADB transport is online.
export function connectionDrawing(online) {
  return svg`<svg class="connection-drawing ${online ? 'linked' : ''}" viewBox="0 0 320 140" aria-hidden="true" focusable="false">
    <defs><pattern id="dots" width="10" height="10" patternUnits="userSpaceOnUse"><circle cx="3" cy="3" r="1" fill="var(--diagram-grid)"/></pattern></defs>
    <rect x="8" y="8" width="304" height="124" rx="24" fill="url(#dots)"/>
    <ellipse cx="159" cy="114" rx="122" ry="8" fill="var(--diagram-shadow)"/>
    <path class="wire" d="M171 103h13q8 0 8 7v4q0 7 8 7h35q9 0 9-9v-12" fill="none" stroke-width="3" stroke-linecap="round"/>
    <g stroke="var(--diagram-ink)" stroke-width="2.4" stroke-linejoin="round">
      <rect x="45" y="29" width="126" height="75" rx="7" fill="var(--diagram-metal)"/>
      <rect x="51" y="35" width="114" height="60" rx="3" fill="var(--diagram-screen)" stroke="none"/>
      <path d="M44 102h127l13 7q0 4-5 4H36q-5 0-5-4z" fill="var(--diagram-metal)"/><path d="M96 103h25l3 3H93z" fill="var(--diagram-ink)" stroke="none"/>
      <rect x="219" y="14" width="51" height="91" rx="10" fill="var(--diagram-metal)"/>
      <rect x="224" y="19" width="41" height="81" rx="6" fill="var(--diagram-screen)" stroke="none"/>
      <path d="M238 21h13m-12 74h11" stroke-linecap="round" stroke-width="2"/>
    </g>
    <g transform="translate(90,46)" stroke="var(--diagram-ink)" stroke-width="2.5" fill="none" stroke-linecap="round"><rect x="4" y="4" width="27" height="27" rx="5" fill="var(--diagram-chip)"/><path d="M0 12h4m-4 10h4m27-10h4m-4 10h4M12 0v4m10-4v4M12 31v4m10-4v4m1-19H12v10h11m-11-5h8"/></g>
    <circle cx="244.5" cy="60" r="12" fill="var(--diagram-chip)"/><path d=${online ? 'm239 60 3.5 3.5 7-8' : 'M240 60h9'} fill="none" stroke="var(--diagram-ink)" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"/>
  </svg>`;
}
