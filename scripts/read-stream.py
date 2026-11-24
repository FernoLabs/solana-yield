#!/usr/bin/env python3
"""Read stored instruction candidates or optional events without network calls."""
import argparse
import json
import sqlite3
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('database', type=Path)
    parser.add_argument('--limit', type=int, default=20)
    parser.add_argument('--kind', choices=('instructions', 'candidates', 'events', 'rejections'), default='instructions')
    parser.add_argument('--rejections', action='store_true', help='Alias for --kind rejections')
    parser.add_argument('--status', choices=('processed', 'confirmed', 'finalized', 'invalidated', 'all'), default='finalized')
    args = parser.parse_args()
    if not 1 <= args.limit <= 1000:
        parser.error('limit must be from 1 to 1000')
    db = sqlite3.connect(args.database.expanduser().resolve().as_uri() + '?mode=ro', uri=True)
    db.row_factory = sqlite3.Row
    bank_columns = {row['name'] for row in db.execute('PRAGMA table_info(stream_banks)')}
    continuity = 'b.continuity_contiguous' if 'continuity_contiguous' in bank_columns else '0'
    state = None if args.status == 'all' else args.status
    kind = 'rejections' if args.rejections else args.kind
    if kind == 'rejections':
        query = f"SELECT b.status,{continuity} AS history_contiguous,b.identity_json AS bank,b.payload_frame_sha256,j.value AS rejection FROM stream_banks b,json_each(b.parsed_json,'$.parser_rejection_details') j WHERE (? IS NULL OR b.status=?) ORDER BY b.payload_sequence DESC LIMIT ?"
    else:
        if kind == 'instructions':
            table, fields, order = 'stream_instructions', 'e.signature,e.transaction_index,e.outer_instruction_index,e.inner_instruction_index,e.stack_height,e.program,e.name,e.execution_status,e.decode_status,e.decode_reason,e.data_base58,e.accounts_json AS accounts,e.arguments_json AS arguments', 'e.transaction_index DESC,e.outer_instruction_index DESC,e.inner_instruction_index DESC'
        elif kind == 'candidates':
            table, fields, order = 'stream_candidates', 'e.signature,e.transaction_index,e.execution_status,e.log_status,e.log_reason,e.parser_version,e.transaction_json AS transaction', 'e.transaction_index DESC'
        else:
            table, fields, order = 'stream_parser_events', 'e.signature,e.transaction_index,e.event_json AS event', 'e.transaction_index DESC,e.event_index DESC'
        # The SQL identifiers above are a fixed local allowlist.
        query = f"SELECT e.status,{continuity} AS history_contiguous,b.identity_json AS bank,b.payload_frame_sha256,json_extract(b.parsed_json,'$.block_time') AS block_time,{fields} FROM {table} e JOIN stream_banks b USING(bank_key) WHERE (? IS NULL OR e.status=?) ORDER BY b.payload_sequence DESC,{order} LIMIT ?"
    for row in db.execute(query, (state, state, args.limit)):
        value = dict(row)
        value['history_contiguous'] = bool(value['history_contiguous'])
        for key in ('bank', 'accounts', 'arguments', 'transaction', 'event', 'rejection'):
            if key in value:
                value[key] = json.loads(value[key])
        if value.get('inner_instruction_index') == -1:
            value['inner_instruction_index'] = None
        print(json.dumps(value, separators=(',', ':')))
    db.close()


if __name__ == '__main__':
    main()
