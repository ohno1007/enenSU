import { html } from 'lit';
import { unsafeSVG } from 'lit/directives/unsafe-svg.js';
import '@material/web/icon/icon.js';
import add from '@material-design-icons/svg/round/add.svg';
import check from '@material-design-icons/svg/round/check.svg';
import chip from '@material-design-icons/svg/round/memory.svg';
import close from '@material-design-icons/svg/round/close.svg';
import device from '@material-design-icons/svg/round/phone_android.svg';
import error from '@material-design-icons/svg/round/error_outline.svg';
import file from '@material-design-icons/svg/round/description.svg';
import flash from '@material-design-icons/svg/round/system_update.svg';
import folder from '@material-design-icons/svg/round/folder_open.svg';
import help from '@material-design-icons/svg/round/help_outline.svg';
import image from '@material-design-icons/svg/round/layers.svg';
import logs from '@material-design-icons/svg/round/terminal.svg';
import refresh from '@material-design-icons/svg/round/refresh.svg';
import shield from '@material-design-icons/svg/round/verified_user.svg';
import usb from '@material-design-icons/svg/round/usb.svg';
const icons = {
  add,
  check,
  chip,
  close,
  device,
  error,
  file,
  flash,
  folder,
  help,
  image,
  logs,
  refresh,
  shield,
  usb,
};
export const icon = (name, slot) =>
  html`<md-icon aria-hidden="true" slot=${slot ?? ''}>${unsafeSVG(icons[name])}</md-icon>`;
