import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Duplex } from 'node:stream';

export interface PortholeAuthOptions {
  /** Use this token instead of `process.env.APP_TOKEN`. */
  token?: string;
  /** With no token, serve every request without auth instead of throwing. */
  allowStandalone?: boolean;
  /** Called once at setup when standalone mode is active. Default: warn on stderr. */
  onStandalone?: () => void;
}

export interface PortholeAuthMiddleware {
  (req: IncomingMessage, res: ServerResponse, next: (err?: unknown) => void): void;
  /** Check a WebSocket upgrade. On false the socket has been answered and closed. */
  verifyUpgrade(req: IncomingMessage, socket: Duplex): boolean;
}

declare function portholeAuth(options?: PortholeAuthOptions): PortholeAuthMiddleware;
export default portholeAuth;
export { portholeAuth };
