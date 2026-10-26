"""Pleat native Hermes entry. Only bounded identity records are loaded here."""
import json
import os
from pathlib import Path
import stat
from .adapter import register as _register_adapter


def _manifest():
    empty={"schemaVersion":1,"skills":[]}
    directory_fd=records_fd=None
    try:
        nofollow=getattr(os,"O_NOFOLLOW",None)
        if nofollow is None:return empty
        root=Path(__file__).parent.resolve(strict=True)
        directory_fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY|nofollow)
        root_identity=os.fstat(directory_fd)
        records_fd=os.open("registrations",os.O_RDONLY|os.O_DIRECTORY|nofollow,dir_fd=directory_fd)
        record_identity=os.fstat(records_fd)
        names=[]
        with os.scandir(records_fd) as children:
            for child in children:
                if len(names)>=4096:return empty
                names.append(child.name)
        entries=[];total=0
        for name in sorted(names):
            if len(name)!=29 or not name.endswith('.json') or any(c not in '0123456789abcdef' for c in name[:-5]):continue
            fd=os.open(name,os.O_RDONLY|os.O_NONBLOCK|nofollow,dir_fd=records_fd)
            try:
                before=os.fstat(fd)
                if not stat.S_ISREG(before.st_mode) or before.st_nlink!=1 or before.st_size>16384:return empty
                raw=os.read(fd,16385);after=os.fstat(fd);observed=os.stat(name,dir_fd=records_fd,follow_symlinks=False)
                fingerprint=lambda value:(value.st_dev,value.st_ino,value.st_size,value.st_mtime_ns,value.st_ctime_ns)
                if len(raw)!=before.st_size or fingerprint(before)!=fingerprint(after) or fingerprint(after)!=fingerprint(observed):return empty
                total+=len(raw)
                if total>8*1024*1024:return empty
                value=json.loads(raw)
                if value.get('manager')!='pleat-native-hermes-v1' or value.get('schemaVersion')!=1 or not isinstance(value.get('skills'),list) or len(value['skills'])!=1:return empty
                entry=value['skills'][0]
                if not isinstance(entry,dict):return empty
                entries.append(entry)
            finally:os.close(fd)
        current_root=os.stat(root,follow_symlinks=False)
        current_records=os.stat('registrations',dir_fd=directory_fd,follow_symlinks=False)
        if (root_identity.st_dev,root_identity.st_ino)!=(current_root.st_dev,current_root.st_ino) or (record_identity.st_dev,record_identity.st_ino)!=(current_records.st_dev,current_records.st_ino):return empty
        return {"schemaVersion":1,"skills":entries}
    except Exception:
        return empty
    finally:
        if records_fd is not None:os.close(records_fd)
        if directory_fd is not None:os.close(directory_fd)


def register(ctx):
    _register_adapter(ctx,_manifest)
