import subprocess
import os
import re

def resolve_file(filepath):
    if not os.path.exists(filepath):
        return
    with open(filepath, 'r', encoding='utf-8', errors='ignore') as f:
        content = f.read()
    if '<<<<<<<' not in content:
        return

    # Match standard conflict blocks
    pattern = re.compile(r'<<<<<<< [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n', re.DOTALL)
    
    def replacer(match):
        ours = match.group(1)
        theirs = match.group(2)
        
        # If one is empty, take the other
        if not ours.strip():
            return theirs
        if not theirs.strip():
            return ours
            
        # If it's imports or attributes or independent functions/tests, union them
        ours_lines = ours.splitlines(keepends=True)
        theirs_lines = theirs.splitlines(keepends=True)
        
        # Union without duplicates preserving order
        res = list(ours_lines)
        for line in theirs_lines:
            if line not in res:
                res.append(line)
        return "".join(res)

    new_content = pattern.sub(replacer, content)
    
    # Also handle diff3 if any (|||||||)
    pattern3 = re.compile(r'<<<<<<< [^\n]*\n.*?\|\|\|\|\|\|\| [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n', re.DOTALL)
    new_content = pattern3.sub(replacer, new_content)

    with open(filepath, 'w', encoding='utf-8') as f:
        f.write(new_content)

def auto_resolve_conflicts():
    status = subprocess.run(['git', 'status', '--porcelain'], stdout=subprocess.PIPE, text=True).stdout
    for line in status.splitlines():
        if line.startswith(('UU ', 'AA ', 'UD ', 'DU ')):
            path = line[3:].strip()
            resolve_file(path)
            subprocess.run(['git', 'add', path])
        elif line.startswith(('DD ', ' D ')):
            path = line[3:].strip()
            subprocess.run(['git', 'rm', '-f', path])
        elif line.startswith((' M ', 'A ', 'M ')):
            path = line[3:].strip()
            subprocess.run(['git', 'add', path])

print("Helper ready")
