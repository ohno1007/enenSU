import { html, render, nothing } from 'lit';
import '@material/web/button/filled-button.js';
import '@material/web/button/filled-tonal-button.js';
import '@material/web/button/text-button.js';
import '@material/web/iconbutton/icon-button.js';
import '@material/web/labs/card/filled-card.js';
import '@material/web/labs/card/outlined-card.js';
import '@material/web/dialog/dialog.js';
import '@material/web/select/outlined-select.js';
import '@material/web/select/select-option.js';
import '@material/web/progress/linear-progress.js';
import brand from '../../icon.svg';
import { unsafeSVG } from 'lit/directives/unsafe-svg.js';
import { icon } from './icons.js';
import { connectionDrawing } from './connection.js';
import './style.css';

const labels = ['设备检查', '镜像修补', '分区写入', '开机验证'];
const stepIcons = ['usb', 'image', 'flash', 'shield'];
const state = {
  devices: [],
  serial: '',
  modules: [],
  firmware: null,
  scanning: true,
  scanError: '',
  busy: false,
  stage: -1,
  status: 'idle',
  detail: '',
  session: '',
  backup: null,
  log: [],
  modal: '',
  lastModal: 'help',
  origin: '',
  fingerprint: '',
  notice: '',
};
const post = (type, data = {}) => window.ipc?.postMessage(JSON.stringify({ type, ...data }));
const selected = () => state.devices.find((d) => d.serial === state.serial);
const actionable = (d) => d && d.state === 'device' && d.locked !== true;
const humanSize = (n) =>
  n >= 1048576 ? `${(n / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`;
const show = (modal) => {
  state.modal = modal;
  if (modal) state.lastModal = modal;
  draw();
};
const log = (text) =>
  state.log.push({ time: new Date().toLocaleTimeString('zh-CN', { hour12: false }), text });
function receive(e) {
  switch (e.type) {
    case 'scanning':
      state.scanning = true;
      break;
    case 'devices': {
      state.scanning = false;
      state.scanError = e.error || '';
      state.devices = e.devices;
      if (!selected()) state.serial = e.devices.length === 1 ? e.devices[0].serial : '';
      break;
    }
    case 'files':
      state.modules = e.modules;
      state.firmware = e.firmware;
      break;
    case 'started':
      Object.assign(state, {
        busy: true,
        stage: 0,
        status: 'running',
        detail: '正在检查设备',
        session: selected()?.model || state.serial,
        backup: null,
        origin: '',
        log: [],
        notice: '',
        scanning: false,
      });
      break;
    case 'progress':
      state.stage = e.stage;
      state.detail = e.message;
      log(e.message);
      if (e.origin) state.origin = `${e.origin} · ${e.target}`;
      if (e.source) {
        state.backup = e.source;
        log(e.source);
        log(e.image);
      }
      break;
    case 'fallback':
      state.fingerprint = e.build;
      state.modal = 'fallback';
      state.lastModal = 'fallback';
      break;
    case 'fallbackClosed':
      state.modal = '';
      break;
    case 'finished':
      state.busy = false;
      state.status = e.status;
      state.detail = e.message;
      state.modal = '';
      log(e.message);
      break;
    case 'notice':
      state.notice = e.message;
      break;
  }
  draw();
}
window.enensuReceive = receive;

function devicePanel() {
  const d = selected(),
    online = d?.state === 'device';
  let heading = '等待连接手机',
    hint = '用 USB 连接手机，打开 USB 调试并在手机上允许此电脑。',
    badge = '未连接';
  if (d) {
    heading = d.model || d.serial;
    const description = {
      unauthorized: ['待授权', '在手机上接受 USB 调试授权，然后刷新设备。'],
      offline: ['离线', '请检查 USB 连接，等待设备重新联机。'],
      recovery: ['恢复模式', '请启动到 Android 后再连接。'],
      sideload: ['Sideload 模式', '请启动到 Android 后再连接。'],
    };
    [badge, hint] = description[d.state] || [
      d.locked === true ? 'BL 已锁定' : 'ADB 已授权',
      d.serial,
    ];
  } else if (state.devices.length > 1) {
    heading = '选择目标手机';
    badge = '多个设备';
    hint = '选择本次要刷入的设备。';
  } else if (state.scanError) {
    heading = '设备扫描失败';
    badge = '连接异常';
    hint = state.scanError;
  }
  return html`<md-filled-card class="connection panel">
    <div class="connection-main">
      <div class="device-title">
        <div class="overline">${icon('device')}设备连接</div>
        <span class="state-pill ${online && d.locked !== true ? 'online' : ''}"
          ><i></i>${badge}</span
        >
      </div>
      <h2>${heading}</h2>
      <p class="device-hint" title=${hint}>${hint}</p>
      ${online
        ? html`<div class="facts">
            <span>内核 <b title=${d.kernel}>${d.kernel || '未知'}</b></span
            ><span>槽位 <b>${d.slot?.toUpperCase() || '未报告'}</b></span
            ><span>BL <b>${d.locked === null ? '待核对' : d.locked ? '已锁定' : '已解锁'}</b></span>
          </div>`
        : nothing}
      <div class="device-actions">
        ${state.devices.length > 1
          ? html`<md-outlined-select
              label="目标设备"
              .value=${state.serial}
              ?disabled=${state.busy}
              @change=${(e) => {
                state.serial = e.target.value;
                draw();
              }}
              >${state.devices.map(
                (phone) =>
                  html`<md-select-option .value=${phone.serial}
                    ><div slot="headline">
                      ${phone.model || phone.serial} · ${phone.serial}
                    </div></md-select-option
                  >`,
              )}</md-outlined-select
            >`
          : nothing}
        <md-text-button
          class="compact"
          ?disabled=${state.busy || state.scanning}
          @click=${() => post('refresh')}
          >${icon('refresh', 'icon')}刷新设备</md-text-button
        >
      </div>
    </div>
    <div class="drawing">${connectionDrawing(online)}</div></md-filled-card
  >`;
}

function inputs() {
  return html`<section class="inputs" aria-label="刷入配置">
    <md-outlined-card class="input panel"
      ><div class="card-heading">
        <span class="tile">${icon('chip')}</span>
        <h2>内核模块</h2>
        <md-filled-tonal-button
          class="card-action"
          ?disabled=${state.busy}
          @click=${() => post('pickModules')}
          >${icon('add', 'icon')}${state.modules.length
            ? '添加模块'
            : '选择模块'}</md-filled-tonal-button
        >
      </div>
      ${state.modules.length
        ? html`<ul class="files">
            ${state.modules.map(
              (m, i) =>
                html`<li>
                  ${icon('file')}<span title=${m.path}>${m.name}</span
                  ><small>${humanSize(m.size)}</small
                  ><md-icon-button
                    aria-label=${'移除 ' + m.name}
                    title=${'移除 ' + m.name}
                    ?disabled=${state.busy}
                    @click=${() => post('removeModule', { index: i })}
                    >${icon('close')}</md-icon-button
                  >
                </li>`,
            )}
          </ul>`
        : html`<p class="input-note">选择为当前内核编译的 <code>.ko</code> 文件，可多选。</p>`}
    </md-outlined-card>
    <md-outlined-card class="input panel"
      ><div class="card-heading">
        <span class="tile tonal">${icon('image')}</span>
        <h2>启动镜像</h2>
        <md-text-button
          class="card-action"
          ?disabled=${state.busy}
          @click=${() => post('pickImage')}
          >${icon('folder', 'icon')}${state.firmware ? '更换镜像' : '选择镜像'}</md-text-button
        >
      </div>
      ${state.firmware
        ? html`<ul class="files">
            <li>
              ${icon('file')}<span title=${state.firmware.path}>${state.firmware.name}</span
              ><md-icon-button
                aria-label="恢复自动回读"
                title="恢复自动回读"
                ?disabled=${state.busy}
                @click=${() => post('clearImage')}
                >${icon('close')}</md-icon-button
              >
            </li>
          </ul>`
        : html`<p class="input-note">
            <strong>默认从设备读取</strong> · 回读受限时可选同版本 <code>.img</code>。
          </p>`}
    </md-outlined-card>
  </section>`;
}

function pipeline() {
  const d = selected(),
    finished = state.status === 'loaded',
    warn = state.status === 'unverified',
    failed = state.status === 'failed';
  const title = finished
    ? '模块加载已验证'
    : warn
      ? '模块加载待验证'
      : failed
        ? '刷入任务未完成'
        : state.busy
          ? '刷入进行中'
          : '刷入进度';
  const note =
    state.busy || state.status !== 'idle'
      ? state.detail
      : !actionable(d)
        ? '连接已授权、已解锁的手机后即可开始。'
        : !state.modules.length
          ? '请先选择一个或多个内核模块。'
          : `已选择 ${state.modules.length} 个模块，将刷入 ${d.model || d.serial} 的当前槽位。`;
  return html`<section class="workflow" aria-label="刷入进度" aria-busy=${state.busy}>
    <div class="flow-head">
      <h2>${title}</h2>
      ${state.session
        ? html`<span class="session">${state.session}</span>`
        : nothing}<md-text-button
        class="log-button"
        ?disabled=${!state.log.length}
        @click=${() => show('logs')}
        >${icon('logs', 'icon')}查看日志</md-text-button
      >
    </div>
    ${state.origin
      ? html`<p class="origin">本次镜像来源 <code>${state.origin}</code></p>`
      : nothing}
    <ol class="steps">
      ${labels.map((label, i) => {
        const prior = i < state.stage || finished,
          current = i === state.stage;
        const cls = prior
          ? 'passed'
          : current
            ? failed
              ? 'failed'
              : warn
                ? 'warning'
                : 'active'
            : '';
        return html`<li class=${cls} aria-current=${current && !finished ? 'step' : 'false'}>
          <span class="step-icon"
            >${icon(prior ? 'check' : current && (failed || warn) ? 'error' : stepIcons[i])}</span
          ><span>${label}</span>
        </li>`;
      })}
    </ol>
    ${state.busy
      ? html`<md-linear-progress indeterminate aria-label="设备操作进行中"></md-linear-progress>`
      : nothing}
    <div class="action-row">
      <p class=${failed ? 'failure' : warn ? 'warning' : ''} role="status">${note}</p>
      <div class="actions">
        ${state.backup
          ? html`<md-text-button @click=${() => post('openBackup')}
              >${icon('folder', 'icon')}打开备份</md-text-button
            >`
          : nothing}
        <md-filled-button
          id="start"
          ?disabled=${state.busy || !actionable(d) || !state.modules.length}
          @click=${() => post('start', { serial: state.serial })}
          >${icon('flash', 'icon')}${state.busy ? '正在刷入' : '修补并刷入'}</md-filled-button
        >
      </div>
    </div>
  </section>`;
}

function dialog() {
  const m = state.modal || state.lastModal,
    fallback = m === 'fallback';
  return html`<md-dialog
    .open=${!!state.modal}
    aria-label=${m === 'help' ? '使用说明' : m === 'logs' ? '操作日志' : '选择同版本镜像'}
    @cancel=${(e) => {
      e.preventDefault();
      if (fallback) post('cancelFallback');
      else show('');
    }}
  >
    <div slot="headline">
      ${m === 'help' ? '使用说明' : m === 'logs' ? '操作日志' : '需要同版本启动镜像'}
    </div>
    <div slot="content">
      ${m === 'help'
        ? html`<p>
              使用已解锁 Bootloader 的手机，打开 USB 调试，并在手机上授权此电脑。Windows
              需能识别手机的 ADB 和 fastboot USB 驱动。
            </p>
            <p>
              选择匹配手机架构和内核 ABI
              的模块，点击「修补并刷入」。如果设备限制回读，请选择当前固件版本的原始镜像。源镜像和修补镜像保存在电脑上。
            </p>
            <p>开机后应用会检查模块加载情况。要求自行重签镜像的设备请使用 enenSU 离线修补流程。</p>
            <p class="meta">enenSU Desktop 0.3.0 · Material Web 2.5.0 · Platform Tools 37.0.3</p>`
        : m === 'logs'
          ? html`<ol class="log-list">
              ${state.log.map((e) => html`<li><time>${e.time}</time><span>${e.text}</span></li>`)}
            </ol>`
          : html`<p>
                设备未开放所需分区的回读。请选择与这台手机当前版本匹配的原始 <code>.img</code>。
              </p>
              <p class="fingerprint">${state.fingerprint}</p>`}
    </div>
    <div slot="actions">
      ${m === 'help'
        ? html`<md-text-button @click=${() => post('openLicenses')}>开源许可</md-text-button>`
        : nothing}
      ${fallback
        ? html`<md-text-button @click=${() => post('cancelFallback')}>取消本次刷入</md-text-button
            ><md-filled-button @click=${() => post('pickFallback')}>选择镜像</md-filled-button>`
        : html`<md-filled-button @click=${() => show('')}>关闭</md-filled-button>`}
    </div></md-dialog
  >`;
}

function draw() {
  render(
    html`<header class="appbar">
        <div class="bar-content">
          <div class="brand">${unsafeSVG(brand)}<b>enenSU</b></div>
          <md-icon-button aria-label="使用说明" title="使用说明" @click=${() => show('help')}
            >${icon('help')}</md-icon-button
          >
        </div>
      </header>
      <main>
        <h1>内核模块刷入</h1>
        ${devicePanel()}${inputs()}${pipeline()}${state.notice
          ? html`<div class="notice" role="alert">
              ${icon('error')}<span>${state.notice}</span
              ><md-icon-button
                aria-label="关闭提示"
                @click=${() => {
                  state.notice = '';
                  draw();
                }}
                >${icon('close')}</md-icon-button
              >
            </div>`
          : nothing}
      </main>
      ${dialog()}`,
    document.getElementById('app'),
  );
}
draw();
post('ready');
setInterval(() => {
  if (!document.hidden && !state.busy && !state.scanning && !state.modal) post('refresh');
}, 6000);
