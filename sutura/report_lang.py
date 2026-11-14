"""Minimal TR localisation for the CLI human report.

The JSON contract is language-neutral; only ``--human --lang tr`` runs this
post-processor. English output is never touched, so existing CLI text stays
byte-identical. Technical identifiers/paths/codes are deliberately kept.
"""
import re

_VERDICTS = {
    'Repaired, shape preserved': 'Onarıldı, şekil korundu',
    'Repaired with notes — shape preserved': 'Notlarla onarıldı — şekil korundu',
    'Repaired, shape changed - check': 'Onarıldı, şekil değişti - kontrol edin',
    'Repaired, shape may be rebuilt - check': 'Onarıldı, şekil yeniden kurulmuş olabilir - kontrol edin',
    'Not fully repaired': 'Tam onarılmadı',
    'Shape check unavailable': 'Şekil kontrolü kullanılamıyor',
    'Safe to inspect': 'İncelemek güvenli',
    'Review recommended': 'İnceleme önerilir',
    'Caution advised': 'Dikkatli olun',
    'Failed / inspect': 'Başarısız / inceleyin',
    'Scoring unavailable': 'Puanlama kullanılamıyor',
}

_LABELS = [
    ('Input :', 'Girdi :'),
    ('Output:', 'Çıktı:'),
    ('Mode  :', 'Mod   :'),
    ('Classifier:', 'Sınıflandırıcı:'),
    ('Profile:', 'Profil:'),
    ('Intensity:', 'Yoğunluk:'),
    ('Type  :', 'Tür   :'),
    ('Confidence:', 'Güven:'),
    ('Health:', 'Sağlık:'),
    ('Risk:', 'Risk:'),
    ('Fidelity:', 'Sadakat:'),
    ('Fidelity note:', 'Sadakat notu:'),
    ('Repair status:', 'Onarım durumu:'),
    ('Budget:', 'Bütçe:'),
    ('Material:', 'Malzeme:'),
    ('Stage 1 (MeshLab):', 'Aşama 1 (MeshLab):'),
    ('Stage 2 (Manifold):', 'Aşama 2 (Manifold):'),
    ('3MF objects repaired:', 'Onarılan 3MF nesnesi:'),
    ('Objects watertight:', 'Su geçirmez nesneler:'),
    ('Methods:', 'Yöntemler:'),
    ('Print impact:', 'Baskı etkisi:'),
    ('Fixed :', 'Düzeltilen:'),
    ('Fixed:', 'Düzeltilen:'),
    ('Remaining:', 'Kalan:'),
    ('Geometry:', 'Geometri:'),
    ('Orientation:', 'Yönelim:'),
    ('Parts:', 'Parçalar:'),
    ('Shape change:', 'Şekil değişimi:'),
    ('Volume:', 'Hacim:'),
    ('Deviation:', 'Sapma:'),
    ('Holes closed', 'Kapatılan delikler'),
    ('Holes remaining', 'Kalan delikler'),
    ('Non-manifold edges fixed', 'Düzeltilen non-manifold kenarlar'),
    ('Faces removed', 'Kaldırılan yüzler'),
    ('Connected components', 'Bağlı bileşenler'),
    ('Two-manifold', 'İki-manifold'),
    ('Vertices', 'Tepe noktaları'),
    ('Faces', 'Yüzler'),
    ('Surface area change', 'Yüzey alanı değişimi'),
    ('Volume change', 'Hacim değişimi'),
    ('Pinched vertices split', 'Sıkışmış tepe noktaları ayrıldı'),
    ('Reload-safe final pass', 'Reload-güvenli son geçiş'),
    ('Extreme passes', 'Ekstrem geçişler'),
    ('Autorefine (experiment)', 'Otomatik iyileştirme (deneysel)'),
    ('Indirect autorefine (experiment)', 'Dolaylı otomatik iyileştirme (deneysel)'),
    ('fTetWild fallback', 'fTetWild yedeği'),
    ('WARNING:', 'UYARI:'),
    ('ERROR:', 'HATA:'),
    ('SKIPPED:', 'ATLANDI:'),
    ('Note:', 'Not:'),
    ('Objects:', 'Nesneler:'),
    ('watertight', 'su geçirmez'),
    ('stage 2 skipped', 'aşama 2 atlandı'),
    ('stage 2 error', 'aşama 2 hatası'),
    ('not kept', 'korunmadı'),
    ('adopted', 'benimsendi'),
    ('no improvement (kept reload-welded state)', 'iyileşme yok (reload-welded durum korundu)'),
    ('within budget', 'bütçe içinde'),
    ('EXCEEDED', 'AŞILDI'),
    ('YES', 'EVET'),
    ('NO', 'HAYIR'),
    ('nothing (input was already sound)', 'yok (girdi zaten sağlamdı)'),
    ('already correct', 'zaten doğru'),
    ('kept', 'korundu'),
    ('reduced budget', 'azaltılmış bütçe'),
    ('geeometric', 'geometrik'),
    ('default thresholds (below confidence gate)',
     'varsayılan eşikler (güven kapısının altında)'),
    ('tuned thresholds', 'ayarlanmış eşikler'),
    ('confidence ', 'güven '),
    ('Medium', 'Orta'),
    ('High', 'Yüksek'),
    ('Low', 'Düşük'),
]


def translate(text, lang='tr'):
    if lang != 'tr':
        return text
    for src, dst in _VERDICTS.items():
        text = text.replace(src, dst)
    for src, dst in (
        ('Orientation:', 'Yönelim:'),
        ('Parts:', 'Parçalar:'),
        ('Shape change:', 'Şekil değişimi:'),
        ('already correct', 'zaten doğru'),
        ('default thresholds (below confidence gate)',
         'varsayılan eşikler (güven kapısının altında)'),
        ('tuned thresholds', 'ayarlanmış eşikler'),
        ('reduced budget', 'azaltılmış bütçe'),
        ('nothing (input was already sound)', 'yok (girdi zaten sağlamdı)'),
        ('Remaining: none', 'Kalan: yok'),
        ('confidence', 'güven'),
        ('deviation max', 'maksimum sapma'),
        (' of size', ' (boyuta göre)'),
        ('volume ', 'hacim '),
        (' kept', ' korundu'),
        ('Fidelity:', 'Sadakat:'),
        ('Medium', 'Orta'),
        ('High', 'Yüksek'),
        ('Low', 'Düşük'),
    ):
        text = text.replace(src, dst)
    out = []
    for line in text.splitlines():
        m = re.match(r'^(\s*)(.*)$', line)
        indent, body = m.group(1), m.group(2)
        for src, dst in _LABELS:
            if body.startswith(src):
                body = dst + body[len(src):]
                break
        out.append(indent + body)
    text = '\n'.join(out)
    text = re.sub(r'Repaired, shape changed in (\d+) area\(s\) - check',
                  r'Onarıldı, şekil \1 alanda değişti - kontrol edin', text)
    text = re.sub(r'Repaired, shape changed in (\d+) guessed area\(s\) - check',
                  r'Onarıldı, tahmini \1 alanda şekil değişti - kontrol edin', text)
    return text
