import subprocess
import os
import re
import sys
import json
import time

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
            
        ours_lines = ours.splitlines(keepends=True)
        theirs_lines = theirs.splitlines(keepends=True)
        
        res = list(ours_lines)
        for line in theirs_lines:
            if line not in res:
                res.append(line)
        return "".join(res)

    new_content = pattern.sub(replacer, content)
    pattern3 = re.compile(r'<<<<<<< [^\n]*\n.*?\|\|\|\|\|\|\| [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n', re.DOTALL)
    new_content = pattern3.sub(replacer, new_content)

    with open(filepath, 'w', encoding='utf-8') as f:
        f.write(new_content)

def auto_resolve_conflicts():
    status = subprocess.run(['git', 'status', '--porcelain'], stdout=subprocess.PIPE, text=True).stdout
    for line in status.splitlines():
        code = line[:2]
        path = line[3:].strip()
        if code in ('UU', 'AA', 'UD', 'DU'):
            resolve_file(path)
            subprocess.run(['git', 'add', path])
        elif 'D' in code:
            subprocess.run(['git', 'rm', '-f', path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        else:
            subprocess.run(['git', 'add', path])

def run():
    # Load remaining PRs
    prs = json.load(open('remaining_prs.json'))
    # Filter out 170 which is already merged
    prs = [p for p in prs if p['number'] != 170]
    
    print(f"Starting batch merge for {len(prs)} PRs...")
    
    merged_count = 0
    failed = []
    
    for i, p in enumerate(prs):
        num = p['number']
        title = p['title']
        author = p['user']['login']
        ref = f"origin/pr/{num}"
        
        print(f"[{i+1}/{len(prs)}] Processing PR #{num} by {author}: {title[:50]}")
        
        # Merge without committing first to inspect
        merge_res = subprocess.run(['git', 'merge', '--no-commit', '--no-ff', ref, '-m', f"Merge pull request #{num} from {author}"],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        
        if merge_res.returncode != 0:
            print(f"  -> Conflicts detected in PR #{num}, auto-resolving...")
            auto_resolve_conflicts()
        
        # Check if there are still unresolved conflicts
        status = subprocess.run(['git', 'status', '--porcelain'], stdout=subprocess.PIPE, text=True).stdout
        unresolved = [line for line in status.splitlines() if line.startswith(('UU', 'AA', 'UD', 'DU'))]
        if unresolved:
            print(f"  -> ERROR: Still unresolved in PR #{num}: {unresolved}")
            subprocess.run(['git', 'merge', '--abort'])
            subprocess.run(['git', 'reset', '--hard', 'HEAD'])
            failed.append((num, 'unresolved conflicts'))
            continue
        
        # Commit the merge
        commit_res = subprocess.run(['git', 'commit', '-m', f"Merge pull request #{num} from {author}\n\nTitle: {title}"],
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if commit_res.returncode != 0 and 'nothing to commit' not in commit_res.stdout:
            print(f"  -> Commit note: {commit_res.stdout.strip() or commit_res.stderr.strip()}")
            
        merged_count += 1
        print(f"  -> Merged PR #{num} successfully.")

    print(f"\nBatch merge complete! Merged: {merged_count}, Failed: {len(failed)}")
    if failed:
        print(f"Failed list: {failed}")

if __name__ == '__main__':
    run()
