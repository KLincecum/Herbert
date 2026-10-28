"""Build the PDF guide from Markdown. Install reportlab to run this script."""
import re
from html import escape
from pathlib import Path
from reportlab.lib import colors
from reportlab.lib.styles import getSampleStyleSheet, ParagraphStyle
from reportlab.platypus import SimpleDocTemplate, Paragraph, Spacer, PageBreak, Table, TableStyle, Image, Preformatted
ROOT=Path(__file__).resolve().parents[1]
styles=getSampleStyleSheet()
for name,size,leading in [('Body',10,15),('Section',22,28),('Sub',13,18),('Cell',9,12),('Cover',48,56)]:
    styles.add(ParagraphStyle(name='G'+name,fontName='Helvetica-Bold' if name in ('Section','Sub','Cover') else 'Helvetica',fontSize=size,leading=leading,textColor=colors.HexColor('#172634'),spaceAfter=10,keepWithNext=name in ('Section','Sub')))
styles.add(ParagraphStyle(name='GCode',fontName='Courier',fontSize=8.5,leading=12,spaceAfter=12,backColor=colors.HexColor('#eef3f6')))
def p(text,kind='Body'):
    text=escape(text)
    text=re.sub(r'\*\*(.+?)\*\*',r'<b>\1</b>',text)
    text=text.replace(chr(96),'')
    text=re.sub(r'\[([^]]+)\]\(([^)]+)\)',r'\1',text)
    return Paragraph(text,styles['G'+kind])
def footer(c,d):
    c.saveState(); c.setFont('Helvetica',8); c.setFillColor(colors.HexColor('#176889'))
    c.drawString(48,28,'HERBERT / USER AND DEVELOPER GUIDE'); c.drawRightString(564,28,str(d.page)); c.restoreState()
source=(ROOT/'docs/Herbert-Guide.md').read_text(encoding='utf-8').replace('—','-').replace('–','-').replace('’',"'")
lines=source.splitlines()
story=[Spacer(1,45),p('Herbert','Cover'),p('Your customizations. Your backup.'),Spacer(1,20),Image(str(ROOT/'assets/branding/herbert-vacuum.png'),width=155,height=155,kind='proportional',hAlign='LEFT'),Spacer(1,30),p('User and developer guide','Section'),p('Connect. Choose your plugins. Download.'),p('Connection settings, collection options, recovery, automation, and building Herbert from source.'),Spacer(1,20),p('Version 0.1.0 | October 2026'),p('Herbert Source-Available License 1.0. Free use and modification; sale and paid hosting require a separate agreement.'),PageBreak(),p('In this guide','Section')]
for line in lines:
    if line.startswith('## '): story.append(p(line[3:]))
story.append(PageBreak())
paragraph=[]
def flush():
    if paragraph: story.append(p(' '.join(paragraph))); paragraph.clear()
i=1
while i<len(lines):
    line=lines[i]
    if line.startswith('## '):
        flush()
        if line[3:] in ('User workflow','Included Epicor collections','Technical interface','Command-line use','Build from source','Troubleshooting','Verification limits','Licensing and notices') and not isinstance(story[-1],PageBreak): story.append(PageBreak())
        else: story.append(Spacer(1,16))
        story.append(p(line[3:],'Section'))
    elif line.startswith('### '): flush(); story.append(p(line[4:],'Sub'))
    elif line.startswith(chr(96)*3):
        flush(); block=[]; i+=1
        while i<len(lines) and not lines[i].startswith(chr(96)*3): block.append(lines[i]); i+=1
        import textwrap
        wrapped='\n'.join('\n'.join(textwrap.wrap(x,95,replace_whitespace=False,drop_whitespace=False)) if len(x)>95 else x for x in block)
        story.append(Preformatted(wrapped,styles['GCode']))
    elif line.startswith('|'):
        flush(); rows=[]
        while i<len(lines) and lines[i].startswith('|'):
            cells=[x.strip() for x in lines[i].strip('|').split('|')]
            if not all(re.fullmatch(r'[: -]+',x) for x in cells): rows.append([p(x,'Cell') for x in cells])
            i+=1
        i-=1
        t=Table(rows,colWidths=[162,354],repeatRows=1)
        t.setStyle(TableStyle([('BACKGROUND',(0,0),(-1,0),colors.HexColor('#dbeaf1')),('VALIGN',(0,0),(-1,-1),'TOP'),('ROWBACKGROUNDS',(0,1),(-1,-1),[colors.white,colors.HexColor('#f3f6f8')]),('TOPPADDING',(0,0),(-1,-1),7),('BOTTOMPADDING',(0,0),(-1,-1),7)])); story.append(t)
    elif re.match(r'^\d+\. ',line) or line.startswith('- '): flush(); story.append(p(line))
    elif not line.strip(): flush()
    else: paragraph.append(line.strip())
    i+=1
flush()
out=ROOT/'docs/Herbert-Guide.pdf'
SimpleDocTemplate(str(out),pagesize=(612,792),leftMargin=48,rightMargin=48,topMargin=50,bottomMargin=60,title='Herbert - User and Developer Guide',author='Herbert').build(story,onFirstPage=footer,onLaterPages=footer)
print(out)
