#!/usr/bin/env python3
"""
What apple/ios/scripts/testflight.sh and the TestFlight workflow ask of the
App Store Connect API after xcodebuild has uploaded a build.

  asc.py uploaded VERSION BUILD    exit 0 when that build is already up
  asc.py wait VERSION BUILD        until Apple has processed it; fails with
                                   Apple's errors, which only this shows
  asc.py publish VERSION BUILD [--notes TEXT] [--group NAME] [--dry-run]
                                   set What to Test, add the build to an
                                   external group and submit it for Beta App
                                   Review; skips what is already done
  asc.py dev-certs                 serial numbers of the Apple Development
                                   certificates in this Mac's keychains
  asc.py revoke-dev-certs --keep FILE
                                   revoke the ones not listed in FILE

The key is testflight.sh's: ASC_KEY_ID, ASC_ISSUER_ID and ASC_KEY_PATH, or
the NOTARY_* ones. The app is DLNZB_BUNDLE_ID (com.zephleggett.dl-nzb); its
record can hold Mac builds too, so only iOS builds count here. Standard
library and openssl only, so /usr/bin/python3 runs it.
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

API = 'https://api.appstoreconnect.apple.com'
BUNDLE_ID = os.environ.get('DLNZB_BUNDLE_ID', 'com.zephleggett.dl-nzb')
PLATFORM = 'IOS'
POLL = 30


class ApiError(Exception):
  pass


def _env(name: str) -> str:
  value = os.environ.get('ASC_' + name) or os.environ.get('NOTARY_' + name)
  if not value:
    sys.exit(f'error: ASC_{name} is not set')
  return value


def _b64(data: bytes) -> str:
  return base64.urlsafe_b64encode(data).rstrip(b'=').decode()


def _der_to_raw(der: bytes) -> bytes:
  """openssl's DER ECDSA signature, SEQUENCE { INTEGER r, INTEGER s }, as the
  64 bytes r || s a JWT carries."""
  i = 2 if der[1] < 0x80 else 2 + (der[1] & 0x7F)
  out = b''
  for _ in range(2):
    n = der[i + 1]
    out += der[i + 2:i + 2 + n].lstrip(b'\x00').rjust(32, b'\x00')
    i += 2 + n
  return out


def _token() -> str:
  now = int(time.time())
  header = {'alg': 'ES256', 'kid': _env('KEY_ID'), 'typ': 'JWT'}
  claims = {'iss': _env('ISSUER_ID'), 'iat': now, 'exp': now + 600, 'aud': 'appstoreconnect-v1'}
  signing = f'{_b64(json.dumps(header).encode())}.{_b64(json.dumps(claims).encode())}'
  der = subprocess.run(['openssl', 'dgst', '-sha256', '-sign', _env('KEY_PATH')],
                       input=signing.encode(), capture_output=True, check=True).stdout
  return f'{signing}.{_b64(_der_to_raw(der))}'


def call(method: str, path: str, body: dict | None = None, ok: tuple = (200, 201, 204)) -> dict:
  data = None if body is None else json.dumps(body).encode()
  req = urllib.request.Request(API + path, data=data, method=method)
  req.add_header('Authorization', 'Bearer ' + _token())
  if data is not None:
    req.add_header('Content-Type', 'application/json')
  try:
    with urllib.request.urlopen(req, timeout=60) as r:
      status, raw = r.status, r.read()
  except urllib.error.HTTPError as e:
    status, raw = e.code, e.read()
  if status not in ok:
    raise ApiError(f'{method} {path}: {status} {raw.decode(errors="replace")}')
  return json.loads(raw) if raw else {}


def get(path: str, **params) -> list:
  """Every item of a list endpoint, following its pages."""
  query = urllib.parse.urlencode(params, safe='[],.')
  path = f'{path}?{query}' if query else path
  items = []
  while path:
    page = call('GET', path)
    items += page.get('data', [])
    path = page.get('links', {}).get('next', '').removeprefix(API)
  return items


def app_id() -> str:
  for app in get('/v1/apps', **{'filter[bundleId]': BUNDLE_ID}):
    if app['attributes']['bundleId'] == BUNDLE_ID:
      return app['id']
  sys.exit(f'error: no app with the bundle ID {BUNDLE_ID} in App Store Connect')


def uploads(app: str, version: str, build: str) -> list:
  """The iOS upload records of that version and build, oldest first. A
  retried upload leaves one per try."""
  found = get(f'/v1/apps/{app}/buildUploads', **{
    'filter[cfBundleShortVersionString]': version, 'filter[cfBundleVersion]': build, 'limit': 50})
  found = [u for u in found if u['attributes'].get('platform', PLATFORM) == PLATFORM]
  return sorted(found, key=lambda u: u['attributes']['createdDate'])


def find_build(app: str, version: str, build: str) -> dict | None:
  found = get('/v1/builds', **{
    'filter[app]': app, 'filter[version]': build, 'filter[preReleaseVersion.version]': version,
    'filter[preReleaseVersion.platform]': PLATFORM})
  return found[0] if found else None


def cmd_uploaded(args) -> int:
  states = [u['attributes']['state']['state'] for u in uploads(app_id(), args.version, args.build)]
  return 0 if {'PROCESSING', 'COMPLETE'} & set(states) else 1


def cmd_wait(args) -> int:
  app = app_id()
  deadline = time.monotonic() + args.timeout
  last = None
  while True:
    try:
      found = uploads(app, args.version, args.build)
      build = find_build(app, args.version, args.build)
    except (ApiError, OSError) as e:
      # a blip in a wait of up to an hour is not Apple's verdict
      if time.monotonic() > deadline:
        raise
      print(f'retrying after: {e}', flush=True)
      time.sleep(POLL)
      continue
    upload = found[-1]['attributes']['state'] if found else None
    state = (build['attributes']['processingState'] if build else
             upload['state'] if upload else 'not listed yet')
    if state != last:
      print(f'{args.version} ({args.build}): {state}', flush=True)
      last = state
    if upload and upload['state'] == 'FAILED':
      for problem in upload['errors']:
        print(f"error: ITMS-{problem.get('code')}: {problem.get('description')}", file=sys.stderr)
      return 1
    if state == 'VALID':
      for problem in upload['warnings'] if upload else []:
        print(f"warning: ITMS-{problem.get('code')}: {problem.get('description')}")
      return 0
    if state in ('FAILED', 'INVALID'):
      print(f'error: App Store Connect marked the build {state}', file=sys.stderr)
      return 1
    if time.monotonic() > deadline:
      print(f'error: still {state} after {args.timeout} s', file=sys.stderr)
      return 1
    time.sleep(POLL)


def cmd_publish(args) -> int:
  app = app_id()
  build = find_build(app, args.version, args.build)
  if not build or build['attributes']['processingState'] != 'VALID':
    sys.exit(f'error: {args.version} ({args.build}) is not a processed build yet; run wait first')
  bid = build['id']

  def act(what: str) -> None:
    print(f'would {what}' if args.dry_run else what)

  if args.notes is not None:
    notes = args.notes.strip()
    existing = [loc for loc in get(f'/v1/builds/{bid}/betaBuildLocalizations') if loc['attributes']['locale'] == 'en-US']
    if existing and existing[0]['attributes'].get('whatsNew') == notes:
      print('What to Test is already set')
    elif existing:
      act('update What to Test')
      if not args.dry_run:
        call('PATCH', f"/v1/betaBuildLocalizations/{existing[0]['id']}", {'data': {
          'type': 'betaBuildLocalizations', 'id': existing[0]['id'], 'attributes': {'whatsNew': notes}}})
    else:
      act('set What to Test')
      if not args.dry_run:
        call('POST', '/v1/betaBuildLocalizations', {'data': {
          'type': 'betaBuildLocalizations', 'attributes': {'locale': 'en-US', 'whatsNew': notes},
          'relationships': {'build': {'data': {'type': 'builds', 'id': bid}}}}})

  if not args.group:
    return 0
  groups = [g for g in get(f'/v1/apps/{app}/betaGroups') if g['attributes']['name'] == args.group]
  if not groups:
    sys.exit(f'error: the app has no TestFlight group named {args.group}')
  group = groups[0]
  if group['attributes']['isInternalGroup']:
    print(f'{args.group} is an internal group: it gets every build without review')
    return 0
  if bid in {b['id'] for b in get(f"/v1/betaGroups/{group['id']}/relationships/builds", limit=200)}:
    print(f'already in {args.group}')
  else:
    act(f'add it to {args.group}')
    if not args.dry_run:
      call('POST', f"/v1/betaGroups/{group['id']}/relationships/builds", {'data': [{'type': 'builds', 'id': bid}]})

  reviews = get('/v1/betaAppReviewSubmissions', **{'filter[build]': bid})
  if reviews:
    print(f"Beta App Review: {reviews[0]['attributes']['betaReviewState']}")
  else:
    act('submit it for Beta App Review')
    if not args.dry_run:
      call('POST', '/v1/betaAppReviewSubmissions', {'data': {
        'type': 'betaAppReviewSubmissions', 'relationships': {'build': {'data': {'type': 'builds', 'id': bid}}}}})
  return 0


def keychain_dev_serials() -> set:
  pems = subprocess.run(['security', 'find-certificate', '-a', '-c', 'Apple Development', '-p'],
                        capture_output=True, text=True).stdout
  serials = set()
  for pem in re.findall(r'-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----', pems, re.S):
    out = subprocess.run(['openssl', 'x509', '-noout', '-serial'], input=pem, capture_output=True, text=True, check=True).stdout
    serials.add(out.strip().partition('=')[2].upper().lstrip('0'))
  return serials


def cmd_dev_certs(args) -> int:
  for serial in sorted(keychain_dev_serials()):
    print(serial)
  return 0


def cmd_revoke_dev_certs(args) -> int:
  with open(args.keep) as f:
    keep = {line.strip().upper().lstrip('0') for line in f if line.strip()}
  failed = 0
  for serial in sorted(keychain_dev_serials() - keep):
    found = get('/v1/certificates', **{'filter[serialNumber]': serial})
    for cert in found:
      kind = cert['attributes']['certificateType']
      if kind not in ('DEVELOPMENT', 'IOS_DEVELOPMENT'):
        print(f'error: {serial} is a {kind} certificate; only development ones are revoked here', file=sys.stderr)
        failed = 1
        continue
      print(f"revoking {cert['attributes'].get('name', 'Apple Development')} {serial}")
      call('DELETE', f"/v1/certificates/{cert['id']}")
    if not found:
      print(f'{serial} is not on the account (already revoked)')
  return failed


def main() -> int:
  parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
  sub = parser.add_subparsers(dest='command', required=True)
  for name, func in (('uploaded', cmd_uploaded), ('wait', cmd_wait), ('publish', cmd_publish)):
    p = sub.add_parser(name)
    p.add_argument('version')
    p.add_argument('build')
    p.set_defaults(func=func)
    if name == 'wait':
      p.add_argument('--timeout', type=int, default=3600, help='seconds (default 3600)')
    if name == 'publish':
      p.add_argument('--notes', help='What to Test, for every tester')
      p.add_argument('--group', help='an external group to add the build to and submit for review')
      p.add_argument('--dry-run', action='store_true', help='say what it would change, change nothing')
  sub.add_parser('dev-certs').set_defaults(func=cmd_dev_certs)
  p = sub.add_parser('revoke-dev-certs')
  p.add_argument('--keep', required=True, help='a file of serial numbers to leave alone, one a line')
  p.set_defaults(func=cmd_revoke_dev_certs)
  args = parser.parse_args()
  try:
    return args.func(args)
  except ApiError as e:
    print(f'error: {e}', file=sys.stderr)
    return 1


if __name__ == '__main__':
  sys.exit(main())
