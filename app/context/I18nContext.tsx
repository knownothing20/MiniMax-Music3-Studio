import React, { createContext, useContext, useState, ReactNode } from 'react';
import { translations, Language, TranslationKey } from '../i18n/translations';

const isLanguage = (value: string | null): value is Language => (
  value === 'zh' || value === 'en' || value === 'ja' || value === 'ko' || value === 'ru'
);

/** Respect an explicit saved choice; a new or invalid preference uses Chinese. */
export function resolveInitialLanguage(stored: string | null): Language {
  return isLanguage(stored) ? stored : 'zh';
}

interface I18nContextType {
  language: Language;
  setLanguage: (lang: Language) => void;
  t: (key: TranslationKey) => string;
}

const I18nContext = createContext<I18nContextType | undefined>(undefined);

export const I18nProvider: React.FC<{ children: ReactNode }> = ({ children }) => {
  const [language, setLanguage] = useState<Language>(() => {
    try {
      return resolveInitialLanguage(localStorage.getItem('language'));
    } catch {
      // A blocked preference store must not stop the studio rendering.
      return 'zh';
    }
  });

  const handleSetLanguage = (lang: Language) => {
    setLanguage(lang);
    localStorage.setItem('language', lang);
  };

  const t = (key: TranslationKey): string => {
    return translations[language][key] || key;
  };

  return (
    <I18nContext.Provider value={{ language, setLanguage: handleSetLanguage, t }}>
      {children}
    </I18nContext.Provider>
  );
};

export const useI18n = () => {
  const context = useContext(I18nContext);
  if (!context) {
    throw new Error('useI18n must be used within I18nProvider');
  }
  return context;
};
