#!/usr/bin/env python3
import bs4
import json

with open('table.html') as fin:
    soup = bs4.BeautifulSoup(fin.read(), 'lxml')

def get_key(s: str) -> str:
    if s == "(Male)":
        return "male"
    elif s == "(Female)":
        return "female"
    return "other"

def get_key2(s: str) -> str:
    return s[1:-1]

table = {}
#re_group = {"female": {}, "male": {}, "other": {}}
re_group = {}

#print(soup.find_all('tr'))
for tr in soup.find_all('tr'):
    code, name, variant = tr.select('td')
    code = code.text
    prev = ''
    for element in variant.children:
        if not isinstance(element, str):
            if element.name == 'code':
                prev = element.text
        else:
            hint = ' '.join(element.strip().split())
            if code not in table:
                table.update({code: []})
            table[code].append({"variant": prev, "hint": hint})
            #print(code, ',', prev, hint)
            key = get_key2(hint)
            if key not in re_group:
                re_group.update({key: {}})
            if code not in re_group[key]:
                re_group[key].update({code: []})
            count = prev.count('-')
            if '-'.join((split := prev.split('-', maxsplit=count + 1))[:count]) == code:
                data = split[-1]
            else:
                data = prev
            re_group[key][code].append(data)
#print(json.dumps(table, ensure_ascii=False, indent='\t', separators=(',', ': ')))
#print(json.dumps(table))
print(json.dumps(re_group))
