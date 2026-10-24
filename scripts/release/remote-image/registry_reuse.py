"""Registry preupload seam: require accepted independent stream result; never upload fallback."""
import json
from pathlib import Path
from guards import bound
import stream_protocol as p
from stream_collect import registry_asset,selected,api

def reuse(spec_path,selection_path,validation_path,receipt_path,accepted_review):
 rawspec=Path(spec_path).read_bytes();s=bound(p.load(rawspec));selection=p.load(Path(selection_path).read_bytes());selected(s,selection,rawspec)
 raw=Path(validation_path).read_bytes();result=p.load(raw);receipt=p.load(Path(receipt_path).read_bytes())
 # Exact independent Mobile review, materialized only after actual stream validation.
 assert accepted_review['accepted'] is True and accepted_review['scope']=='IMAGE_EXPORT_STREAM' and accepted_review['source']==s['source'] and accepted_review['pin_sha256']==s['pin_sha256']
 assert accepted_review['validation_sha256']==p.sha(raw) and accepted_review['run_id']==selection['run_id']
 assert result['registry_asset']['asset_id']==receipt['asset_id'] and result['registry_asset']['release_id']==receipt['release_id']
 asset=api('releases/assets/'+str(receipt['asset_id']));release=api('releases/'+str(receipt['release_id']))
 exact=registry_asset(s,selection,result,receipt,asset,release)
 assert exact==result['registry_asset']
 # The existing selected registry15 binder consumes this asset descriptor, not a local filename.
 # No upload method or local archive read fallback exists in this capability.
 return exact

def publication_inputs(spec_path,selection_path,validation_path,receipt_path,accepted_review):
 """Concrete replacement for local archive hashing/upload in the existing registry15 dispatcher."""
 exact=reuse(spec_path,selection_path,validation_path,receipt_path,accepted_review)
 receipt=p.load(Path(receipt_path).read_bytes())
 return {'version':exact['version'],'source_commit':exact['source'],'input_release':receipt['tag'],'image_sha256':exact['sha256'],'approve_publication':'true'}

def published_binding(raw_manifest,config_blob,archive_config,stream_result,s,run,selected_registry_run):
 """Replace tar reopening in the existing registry binder with independently streamed config bytes."""
 assert type(selected_registry_run)is int and selected_registry_run>0 and run['id']==selected_registry_run
 assert run['run_attempt']==1 and run['head_sha']==s['source'] and run['path']=='.github/workflows/container.yml' and run['event']=='workflow_dispatch' and run['status']=='completed' and run['conclusion']=='success' and run['repository']['full_name']==p.REPO
 assert stream_result['independent_archive_stream_verified'] is True and stream_result['source']==s['source'] and stream_result['version']==s['version'] and stream_result['pin_sha256']==s['pin_sha256'] and stream_result['authority_sha256']==s['authority_sha256']
 manifest=p.load(raw_manifest);assert manifest['schemaVersion']==2
 descriptor=manifest['config'];assert descriptor['digest']=='sha256:'+p.sha(config_blob) and descriptor['size']==len(config_blob)
 assert config_blob==archive_config and p.sha(archive_config)==stream_result['config_sha256']
 config=p.load(config_blob);labels=config['config']['Labels'];assert labels['org.opencontainers.image.revision']==s['source'] and labels['org.opencontainers.image.version']==s['version']
 assert stream_result['all_layer_hashes_verified'] is True and len(manifest['layers'])==stream_result['layer_count']
 # Registry layer descriptors can be compressed; the config's rootfs diff_ids already bind
 # every uncompressed layer from the independently streamed archive. No byte-equality of
 # registry compression versus Docker-save tar is invented.
 return {'target':'ghcr.io/awpsec/kindred-server@sha256:'+p.sha(raw_manifest),'config':descriptor['digest'],'source':s['source'],'workflow':selected_registry_run,'published_config_matches_independently_streamed_archive':True}
