import { Button } from '@cloudflare/kumo/components/button';
import { Dialog } from '@cloudflare/kumo/components/dialog';
import { Input } from '@cloudflare/kumo/components/input';
import { Effect } from 'effect';
import { useEffect, useRef, useState, type FormEvent } from 'react';

import {
  createClient,
  normalizeBaseUrl,
  validateBaseUrl,
  type ClientConfig,
} from '../api/client';
import { errorText } from '../api/fetcher';
import { validateWorkspacePath } from '../domain/workspace';

export interface WorkspaceDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onAdd: (path: string) => void;
}

export function WorkspaceDialog({
  open,
  onOpenChange,
  onAdd,
}: WorkspaceDialogProps) {
  const [path, setPath] = useState('');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setPath('');
      setError(null);
    }
  }, [open]);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const value = path.trim();
    const problem = validateWorkspacePath(value);
    if (problem !== null) {
      setError(problem);
      return;
    }
    onAdd(value);
    onOpenChange(false);
  };

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog className="dialog">
        <Dialog.Title>Add a workspace</Dialog.Title>
        <Dialog.Description>
          Point at an absolute directory on the host that runs the control
          plane.
        </Dialog.Description>
        <form className="dialog-form" onSubmit={submit} noValidate>
          <Input
            label="Workspace path"
            placeholder="/home/me/project"
            value={path}
            onChange={(event) => {
              setPath(event.target.value);
              setError(null);
            }}
            autoFocus
            {...(error === null ? {} : { error })}
          />
          <div className="dialog-actions">
            <Button
              type="button"
              variant="secondary"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit" variant="primary">
              Add workspace
            </Button>
          </div>
        </form>
      </Dialog>
    </Dialog.Root>
  );
}

function originOf(url: string): string | null {
  try {
    return normalizeBaseUrl(url);
  } catch {
    return null;
  }
}

export interface SettingsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  initial: ClientConfig;
  initialError?: string | null;
  onSave: (config: ClientConfig) => void;
}

export function SettingsDialog({
  open,
  onOpenChange,
  initial,
  initialError = null,
  onSave,
}: SettingsDialogProps) {
  const [baseUrl, setBaseUrl] = useState(initial.baseUrl);
  const [token, setToken] = useState(initial.token ?? '');
  const [tokenOrigin, setTokenOrigin] = useState<string | null>(
    originOf(initial.baseUrl),
  );
  const [error, setError] = useState<string | null>(null);
  const [tokenCleared, setTokenCleared] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const openingToken = initial.token ?? '';
  const openingOrigin = originOf(initial.baseUrl);
  const lastTokens = useRef(new Map<string, string>());

  useEffect(() => {
    if (open) {
      lastTokens.current = new Map();
      if (openingOrigin !== null && openingToken.length > 0) {
        lastTokens.current.set(openingOrigin, openingToken);
      }
      setBaseUrl(initial.baseUrl);
      setToken(initial.token ?? '');
      setTokenOrigin(originOf(initial.baseUrl));
      setError(initialError ?? null);
      setTokenCleared(false);
      setResult(null);
      setTesting(false);
    }
  }, [
    open,
    initial.baseUrl,
    initial.token,
    initialError,
    openingOrigin,
    openingToken,
  ]);

  const changeBaseUrl = (value: string) => {
    setBaseUrl(value);
    setError(null);
    const nextOrigin = originOf(value);
    if (nextOrigin === null) {
      return;
    }
    const remembered = lastTokens.current.get(nextOrigin);
    if (remembered !== undefined) {
      setToken(remembered);
      setTokenOrigin(nextOrigin);
      setTokenCleared(false);
    }
  };

  const rememberToken = (value: string) => {
    setToken(value);
    setTokenCleared(false);
    const origin = originOf(baseUrl) ?? tokenOrigin;
    if (origin !== null && value.trim().length > 0) {
      lastTokens.current.set(origin, value);
    }
  };

  const reconcileToken = (origin: string): string | null => {
    if (origin !== tokenOrigin) {
      if (token.length > 0) {
        setTokenCleared(true);
      }
      setToken('');
      setTokenOrigin(origin);
      return null;
    }
    const value = token.trim();
    return value.length > 0 ? value : null;
  };

  const test = async () => {
    const problem = validateBaseUrl(baseUrl);
    if (problem !== null) {
      setError(problem);
      return;
    }
    setTesting(true);
    setResult(null);
    const origin = normalizeBaseUrl(baseUrl);
    const currentToken = reconcileToken(origin);
    try {
      const client = createClient({
        baseUrl: origin,
        token: currentToken,
      });
      const agents = await Effect.runPromise(client.fetchAgents());
      setResult(`Connected to ${agents.agents.length} agent(s).`);
    } catch (caught) {
      setResult(errorText(caught));
    } finally {
      setTesting(false);
    }
  };

  const save = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const problem = validateBaseUrl(baseUrl);
    if (problem !== null) {
      setError(problem);
      return;
    }
    const origin = normalizeBaseUrl(baseUrl);
    onSave({ baseUrl: origin, token: reconcileToken(origin) });
    onOpenChange(false);
  };

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog className="dialog">
        <Dialog.Title>Connection settings</Dialog.Title>
        <Dialog.Description>
          The token is kept in this tab&apos;s sessionStorage and sent as a
          bearer header, never in a URL.
        </Dialog.Description>
        <form className="dialog-form" onSubmit={save} noValidate>
          <Input
            label="Control plane URL"
            placeholder="Same origin"
            value={baseUrl}
            onChange={(event) => changeBaseUrl(event.target.value)}
            onBlur={() => {
              const origin = originOf(baseUrl);
              if (origin !== null) {
                reconcileToken(origin);
              }
            }}
            autoComplete="off"
            {...(error === null ? {} : { error })}
          />
          <Input
            label="Bearer token"
            type="password"
            placeholder="Unset"
            value={token}
            onChange={(event) => rememberToken(event.target.value)}
            autoComplete="off"
          />
          {tokenCleared ? (
            <p className="settings-note">
              The token was cleared because the origin changed.
            </p>
          ) : null}
          {result === null ? null : <p className="settings-result">{result}</p>}
          <div className="dialog-actions">
            <Button
              type="button"
              variant="secondary"
              onClick={() => void test()}
              disabled={testing}
            >
              {testing ? 'Testing...' : 'Test connection'}
            </Button>
            <Button type="submit" variant="primary">
              Save
            </Button>
          </div>
        </form>
      </Dialog>
    </Dialog.Root>
  );
}

export interface ConfirmRemoveDialogProps {
  open: boolean;
  path: string | null;
  unpin: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}

export function ConfirmRemoveDialog({
  open,
  path,
  unpin,
  onOpenChange,
  onConfirm,
}: ConfirmRemoveDialogProps) {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange} role="alertdialog">
      <Dialog className="dialog">
        <Dialog.Title>
          {unpin ? 'Unpin workspace?' : 'Remove workspace?'}
        </Dialog.Title>
        <Dialog.Description>
          {path === null
            ? 'This workspace is kept in the saved list. Continue?'
            : unpin
              ? `Unpin ${path}? Its tasks remain on the daemon and stay listed.`
              : `Remove ${path} from the saved list? Tasks already in it remain and are still listed.`}
        </Dialog.Description>
        <div className="dialog-actions">
          <Button
            type="button"
            variant="secondary"
            onClick={() => onOpenChange(false)}
          >
            Cancel
          </Button>
          <Button type="button" variant="destructive" onClick={onConfirm}>
            {unpin ? 'Unpin' : 'Remove'}
          </Button>
        </div>
      </Dialog>
    </Dialog.Root>
  );
}
