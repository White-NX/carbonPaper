import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  AlertCircle,
  Check,
  CheckCircle2,
  ChevronRight,
  Circle,
  ClipboardCheck,
  Clock3,
  Copy,
  Globe,
  Languages,
  Layers,
  Loader2,
  Lock,
  PlayCircle,
  Power,
  RotateCcw,
  Search,
  ShieldCheck,
  History,
} from 'lucide-react';
import appIcon from '../../../src-tauri/icons/128x128.png';
import { changeAppLanguage } from '../../i18n';
import { cn } from '../../lib/utils';
import { MODEL_DOWNLOAD_MB } from '../../lib/modelSizes';
import { OverlayShell, ProgressBlock } from '../overlay';
import { Button, focusStyle } from '../ui/Button';
import { Banner } from '../ui/Banner';
import { OptionCardGroup, SwitchRow, ToggleCard } from './OptionCards';
import { RERANKER_MODEL_ID, WIZARD_STEPS, needsReranker, selectedBrowsers } from './onboardingPlan';

/** The app's own icon, drawn in the header tile in place of a line icon. */
function AppIcon() {
  return <img src={appIcon} alt="" aria-hidden="true" className="h-7 w-7" />;
}

const STEP_ICONS = { welcome: AppIcon, features: Layers, browserStartup: Globe, review: ClipboardCheck };

function StepIndicator({ step, onSelect, locked }) {
  const { t } = useTranslation();
  return (
    <ol className="flex items-center gap-2" aria-label={t('onboarding.stepsLabel')}>
      {WIZARD_STEPS.map((id, index) => {
        const done = index < step;
        const current = index === step;
        return (
          <li key={id} className="flex min-w-0 flex-1 items-center gap-2">
            <button type="button" disabled={locked || index > step} onClick={() => onSelect(index)}
              aria-current={current ? 'step' : undefined}
              className={cn('flex min-w-0 items-center gap-2 rounded-md text-xs transition-colors disabled:cursor-default', focusStyle,
                current ? 'font-medium text-ide-text' : done ? 'text-ide-muted hover:text-ide-text' : 'text-ide-muted/60')}>
              <span className={cn('flex h-6 w-6 shrink-0 items-center justify-center rounded-full border text-[11px] tabular-nums transition-colors',
                current && 'border-ide-accent bg-ide-accent text-white',
                done && 'border-ide-accent/50 bg-ide-accent/10 text-ide-accent',
                !current && !done && 'border-ide-border')}>
                {done ? <CheckCircle2 className="h-3.5 w-3.5" aria-hidden="true" /> : index + 1}
              </span>
              <span className="truncate">{t(`onboarding.steps.${id}`)}</span>
            </button>
            {index < WIZARD_STEPS.length - 1 && (
              <span className={cn('h-px min-w-3 flex-1', done ? 'bg-ide-accent/50' : 'bg-ide-border')} />
            )}
          </li>
        );
      })}
    </ol>
  );
}

function LanguageSwitch() {
  const { t, i18n } = useTranslation();
  const languages = [['zh-CN', '中文'], ['en', 'English']];
  return (
    <div className="flex items-center gap-1 rounded-lg border border-ide-border bg-ide-bg p-0.5" role="group" aria-label={t('onboarding.welcome.language')}>
      <Languages className="ml-1.5 h-3.5 w-3.5 text-ide-muted" aria-hidden="true" />
      {languages.map(([value, label]) => (
        <button key={value} type="button" aria-pressed={i18n.language === value} onClick={() => changeAppLanguage(value)}
          className={cn('rounded-md px-2 py-1 text-xs transition-colors', focusStyle,
            i18n.language === value ? 'bg-ide-panel font-medium text-ide-text shadow-sm' : 'text-ide-muted hover:text-ide-text')}>
          {label}
        </button>
      ))}
    </div>
  );
}

/**
 * What the app does and what "use recommended settings" will turn on. The
 * page only informs; the two ways forward are the footer buttons, where every
 * step keeps its main action.
 */
function WelcomeStep({ onboarding, downloadMb }) {
  const { t } = useTranslation();
  const { recommended } = onboarding;
  const points = [
    ['record', History],
    ['search', Search],
    ['private', Lock],
  ];
  const includes = [
    'smart',
    'performance',
    recommended && selectedBrowsers(recommended).length > 0 && 'browsers',
    'autostart',
    recommended?.appBound && 'appBound',
  ].filter(Boolean);
  return (
    <div className="space-y-6">
      <ul className="grid gap-5 sm:grid-cols-3">
        {points.map(([id, Icon]) => (
          <li key={id}>
            <span className="flex h-9 w-9 items-center justify-center rounded-full bg-ide-accent/10 text-ide-accent">
              <Icon className="h-4 w-4" aria-hidden="true" />
            </span>
            <p className="mt-3 text-sm font-medium text-ide-text">{t(`onboarding.welcome.points.${id}.title`)}</p>
            <p className="mt-1 text-xs leading-relaxed text-ide-muted">{t(`onboarding.welcome.points.${id}.description`)}</p>
          </li>
        ))}
      </ul>

      <section className="rounded-xl bg-ide-bg p-4">
        <h3 className="text-xs font-medium text-ide-text">{t('onboarding.welcome.recommended.summaryTitle')}</h3>
        <ul className="mt-2.5 grid gap-x-6 gap-y-2 sm:grid-cols-2">
          {includes.map((id) => (
            <li key={id} className="flex items-center gap-2 text-xs text-ide-text">
              <Check className="h-3.5 w-3.5 shrink-0 text-ide-accent" aria-hidden="true" />
              {t(`onboarding.welcome.recommended.items.${id}`)}
            </li>
          ))}
        </ul>
        <p className="mt-3 text-xs leading-relaxed text-ide-muted">
          {downloadMb > 0
            ? t('onboarding.welcome.recommended.noteWithDownload', { size: downloadMb })
            : t('onboarding.welcome.recommended.note')}
        </p>
      </section>
    </div>
  );
}

/** A group's question, with an optional line under it. */
function SectionHeading({ title, hint }) {
  return (
    <div className="mb-3">
      <h3 className="text-sm font-medium text-ide-text">{title}</h3>
      {hint && <p className="mt-0.5 text-xs leading-relaxed text-ide-muted">{hint}</p>}
    </div>
  );
}

function FeaturesStep({ onboarding }) {
  const { t } = useTranslation();
  const { choices, context, setChoice } = onboarding;
  const featureOptions = ['minimal', 'basic', 'smart'].map((value) => ({
    value,
    label: t(`onboarding.featureModes.${value}.label`),
    description: t(`onboarding.featureModes.${value}.description`),
    badge: value === 'smart' ? t('onboarding.recommendedBadge') : undefined,
    note: value === 'smart' && !context.rerankerInstalled
      ? t('onboarding.features.extraDownload', { size: MODEL_DOWNLOAD_MB[RERANKER_MODEL_ID] })
      : undefined,
  }));
  const policyOptions = ['complete', 'balanced', 'performance'].map((value) => ({
    value,
    label: t(`onboarding.policies.${value}.label`),
    description: t(`onboarding.policies.${value}.description`),
    badge: value === 'performance' ? t('onboarding.recommendedBadge') : undefined,
  }));
  return (
    <div className="space-y-7">
      <section>
        <SectionHeading title={t('onboarding.features.modeLabel')} />
        <OptionCardGroup label={t('onboarding.features.modeLabel')} value={choices.featureMode}
          options={featureOptions} onChange={(value) => setChoice('featureMode', value)} />
      </section>
      <section>
        <SectionHeading title={t('onboarding.features.policyLabel')} />
        <OptionCardGroup label={t('onboarding.features.policyLabel')} value={choices.resourcePolicy}
          options={policyOptions} onChange={(value) => setChoice('resourcePolicy', value)} />
      </section>
    </div>
  );
}

function BrowserStartupStep({ onboarding }) {
  const { t } = useTranslation();
  const { choices, context, setChoice, setBrowser } = onboarding;
  return (
    <div className="space-y-7">
      <section>
        <SectionHeading title={t('onboarding.browserStartup.browsersLabel')} hint={t('onboarding.browserStartup.browsersHint')} />
        <div className="grid gap-3 sm:grid-cols-2">
          {['chrome', 'edge'].map((browser) => (
            <ToggleCard key={browser} checked={choices.browsers[browser]}
              onChange={(on) => setBrowser(browser, on)}
              label={t(`onboarding.browsers.${browser}`)}
              description={context.browsers?.[browser]
                ? t('onboarding.browserStartup.detected')
                : t('onboarding.browserStartup.notDetected')} />
          ))}
        </div>
      </section>
      <section>
        <SectionHeading title={t('onboarding.browserStartup.startupLabel')} />
        <div className="divide-y divide-ide-border/60 rounded-xl border border-ide-border bg-ide-bg px-3">
          <SwitchRow icon={Power} checked={choices.launchAtLogin} onChange={(on) => setChoice('launchAtLogin', on)}
            label={t('onboarding.browserStartup.launchAtLogin.label')}
            description={t('onboarding.browserStartup.launchAtLogin.description')} />
          <SwitchRow icon={PlayCircle} checked={choices.autoStartCapture} onChange={(on) => setChoice('autoStartCapture', on)}
            label={t('onboarding.browserStartup.autoStartCapture.label')}
            description={t('onboarding.browserStartup.autoStartCapture.description')} />
          {context.appBoundAvailable && (
            <SwitchRow icon={ShieldCheck} checked={choices.appBound} onChange={(on) => setChoice('appBound', on)}
              label={t('onboarding.appBound.label')}
              description={t('onboarding.appBound.description')}
              note={t('onboarding.appBound.note')} />
          )}
        </div>
      </section>
    </div>
  );
}

function SummaryRow({ label, value, onEdit }) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center justify-between gap-3 py-2 text-sm">
      <span className="text-ide-muted">{label}</span>
      <span className="flex min-w-0 items-center gap-2">
        <span className="truncate text-right text-ide-text">{value}</span>
        {onEdit && (
          <button type="button" onClick={onEdit} className={cn('shrink-0 rounded text-xs text-ide-accent hover:underline', focusStyle)}>
            {t('onboarding.review.edit')}
          </button>
        )}
      </span>
    </div>
  );
}

function DownloadLine({ label, children }) {
  return (
    <div className="space-y-1.5">
      <p className="text-xs font-medium text-ide-text">{label}</p>
      {children}
    </div>
  );
}

function Downloads({ onboarding, required }) {
  const { t } = useTranslation();
  const { choices, context, reranker, phase } = onboarding;
  const wantsReranker = needsReranker(choices, context.rerankerInstalled) || reranker.status !== 'idle';
  if (!required.needDownload && !wantsReranker) return null;

  let requiredBody;
  if (!required.needDownload) requiredBody = <p className="text-xs text-ide-info-success">{t('onboarding.review.downloadReady')}</p>;
  else if (required.error) {
    requiredBody = (
      <Banner tone="error" action={<Button size="xs" icon={RotateCcw} onClick={required.retry}>{t('onboarding.actions.retry')}</Button>}>
        {required.error}
      </Banner>
    );
  } else requiredBody = <ProgressBlock percent={Math.round(required.progress)} />;

  let rerankerBody = null;
  if (wantsReranker) {
    if (reranker.status === 'idle') rerankerBody = <p className="text-xs text-ide-muted">{t('onboarding.review.downloadAfterFinish')}</p>;
    else if (reranker.status === 'queued') rerankerBody = <p className="text-xs text-ide-muted">{t('onboarding.review.downloadQueued')}</p>;
    else if (reranker.status === 'downloading') rerankerBody = <ProgressBlock percent={reranker.percent ?? undefined} />;
    else if (reranker.status === 'done') rerankerBody = <p className="text-xs text-ide-info-success">{t('onboarding.review.downloadReady')}</p>;
    else {
      rerankerBody = (
        <Banner tone="error" action={<Button size="xs" icon={RotateCcw} onClick={reranker.retry}>{t('onboarding.actions.retry')}</Button>}>
          {reranker.error}
        </Banner>
      );
    }
  }

  return (
    <section className="space-y-3 rounded-xl border border-ide-border bg-ide-bg p-3">
      <p className="text-xs font-medium uppercase tracking-wide text-ide-muted">
        {phase === 'edit' ? t('onboarding.review.downloadsTitle') : t('onboarding.review.downloadsProgressTitle')}
      </p>
      {(required.needDownload || phase === 'edit') && (
        <DownloadLine label={t('onboarding.review.requiredComponents', { size: required.sizeMb })}>{requiredBody}</DownloadLine>
      )}
      {rerankerBody && (
        <DownloadLine label={t('onboarding.review.smartComponent', { size: MODEL_DOWNLOAD_MB[RERANKER_MODEL_ID] })}>{rerankerBody}</DownloadLine>
      )}
    </section>
  );
}

const TASK_ICONS = {
  running: <Loader2 className="h-4 w-4 animate-spin text-ide-accent" aria-hidden="true" />,
  done: <CheckCircle2 className="h-4 w-4 text-ide-info-success" aria-hidden="true" />,
  failed: <AlertCircle className="h-4 w-4 text-ide-error" aria-hidden="true" />,
  queued: <Clock3 className="h-4 w-4 text-ide-muted" aria-hidden="true" />,
  pending: <Circle className="h-4 w-4 text-ide-border" aria-hidden="true" />,
};

function TaskList({ onboarding }) {
  const { t } = useTranslation();
  const { tasks, taskStatus, reranker, retryTask } = onboarding;
  return (
    <ul className="divide-y divide-ide-border/60 rounded-xl border border-ide-border bg-ide-bg px-3" aria-live="polite">
      {tasks.map((task) => {
        let status = taskStatus[task.id]?.status ?? 'pending';
        let error = taskStatus[task.id]?.error;
        let retry = () => retryTask(task.id);
        if (task.id === 'reranker' && status === 'done') {
          status = { idle: 'queued', queued: 'queued', downloading: 'running', done: 'done', failed: 'failed' }[reranker.status];
          error = reranker.error;
          retry = reranker.retry;
        }
        return (
          <li key={task.id} className="flex items-start gap-3 py-2.5">
            <span className="mt-0.5 shrink-0">{TASK_ICONS[status]}</span>
            <div className="min-w-0 flex-1">
              <p className="text-sm text-ide-text">{t(`onboarding.tasks.${task.id}`)}</p>
              {status === 'queued' && task.background && <p className="text-xs text-ide-muted">{t('onboarding.review.backgroundNote')}</p>}
              {status === 'failed' && error && <p className="mt-0.5 break-words text-xs text-ide-error">{error}</p>}
            </div>
            {status === 'failed' && <Button size="xs" icon={RotateCcw} onClick={retry}>{t('onboarding.actions.retry')}</Button>}
          </li>
        );
      })}
    </ul>
  );
}

function ExtensionPath({ path }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  if (!path) return null;
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(path);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Clipboard can be unavailable; the path stays selectable.
    }
  };
  return (
    <Banner tone="info" icon={Globe}>
      <p>{t('onboarding.review.extensionPath')}</p>
      <div className="mt-2 flex items-center gap-2">
        <code className="min-w-0 flex-1 select-all break-all rounded bg-ide-bg px-2 py-1 font-mono text-[11px] text-ide-text">{path}</code>
        <Button size="xs" icon={copied ? CheckCircle2 : Copy} onClick={copy}>
          {copied ? t('onboarding.review.copied') : t('onboarding.review.copyPath')}
        </Button>
      </div>
    </Banner>
  );
}

function ReviewStep({ onboarding, required }) {
  const { t, i18n } = useTranslation();
  const { choices, context, phase, goTo, extensionPath } = onboarding;
  const onOff = (on) => (on ? t('onboarding.review.on') : t('onboarding.review.off'));
  const browsers = ['chrome', 'edge'].filter((id) => choices.browsers[id]).map((id) => t(`onboarding.browsers.${id}Short`));
  const featuresIndex = WIZARD_STEPS.indexOf('features');
  const startupIndex = WIZARD_STEPS.indexOf('browserStartup');

  if (phase === 'edit') {
    return (
      <div className="space-y-4">
        <div className="divide-y divide-ide-border/60 rounded-xl border border-ide-border bg-ide-bg px-3">
          <SummaryRow label={t('onboarding.review.rows.featureMode')} value={t(`onboarding.featureModes.${choices.featureMode}.label`)} onEdit={() => goTo(featuresIndex)} />
          <SummaryRow label={t('onboarding.review.rows.resourcePolicy')} value={t(`onboarding.policies.${choices.resourcePolicy}.label`)} onEdit={() => goTo(featuresIndex)} />
          <SummaryRow label={t('onboarding.review.rows.browsers')} value={browsers.length ? new Intl.ListFormat(i18n.language, { type: 'conjunction' }).format(browsers) : t('onboarding.review.none')} onEdit={() => goTo(startupIndex)} />
          <SummaryRow label={t('onboarding.review.rows.launchAtLogin')} value={onOff(choices.launchAtLogin)} onEdit={() => goTo(startupIndex)} />
          <SummaryRow label={t('onboarding.review.rows.autoStartCapture')} value={onOff(choices.autoStartCapture)} onEdit={() => goTo(startupIndex)} />
          {context.appBoundAvailable && (
            <SummaryRow label={t('onboarding.review.rows.appBound')} value={onOff(choices.appBound)} onEdit={() => goTo(startupIndex)} />
          )}
        </div>
        <Downloads onboarding={onboarding} required={required} />
        <p className="text-xs leading-relaxed text-ide-muted">
          {choices.autoStartCapture ? t('onboarding.review.captureStarts') : t('onboarding.review.captureManual')}
        </p>
        {context.appBoundAvailable && choices.appBound && <Banner tone="warning">{t('onboarding.appBound.note')}</Banner>}
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <TaskList onboarding={onboarding} />
      <Downloads onboarding={onboarding} required={required} />
      <ExtensionPath path={extensionPath} />
    </div>
  );
}

/**
 * The first-run wizard. Four steps: welcome (with "use recommended
 * settings"), features and performance, browser and startup, review and apply.
 */
export default function OnboardingWizard({ onboarding, required, downloadMb }) {
  const { t } = useTranslation();
  const { step, stepId, direction, phase } = onboarding;
  const editing = phase === 'edit';
  const resettable = stepId === 'features' || stepId === 'browserStartup';

  let headerKey = stepId;
  if (stepId === 'review' && phase !== 'edit') headerKey = phase === 'applied' ? 'applied' : 'applying';

  let footer = null;
  if (stepId === 'welcome') {
    footer = (
      <>
        <Button size="md" onClick={onboarding.next}>{t('onboarding.welcome.custom.action')}</Button>
        <Button size="md" variant="primary" onClick={onboarding.applyRecommended}>
          {t('onboarding.welcome.recommended.action')}
          <ChevronRight className="-mr-1 h-4 w-4" aria-hidden="true" />
        </Button>
      </>
    );
  } else if (stepId === 'features' || stepId === 'browserStartup') {
    footer = <Button size="md" variant="primary" onClick={onboarding.next}>{t('onboarding.actions.next')}</Button>;
  } else if (stepId === 'review') {
    if (phase === 'edit') footer = <Button size="md" variant="primary" icon={CheckCircle2} onClick={onboarding.apply}>{t('onboarding.actions.finish')}</Button>;
    else if (phase === 'applying') footer = <Button size="md" variant="primary" loading>{t('onboarding.actions.applying')}</Button>;
    else footer = <Button size="md" variant="primary" onClick={onboarding.finish}>{t('onboarding.actions.start')}</Button>;
  }

  return (
    <OverlayShell
      size="xl"
      onDismiss={editing && step > 0 ? onboarding.back : undefined}
      topSlot={<StepIndicator step={step} onSelect={onboarding.goTo} locked={!editing} />}
      icon={headerKey === 'applied' ? CheckCircle2 : STEP_ICONS[stepId]}
      title={t(`onboarding.headers.${headerKey}.title`)}
      subtitle={t(`onboarding.headers.${headerKey}.subtitle`)}
      headerAside={stepId === 'welcome' ? <LanguageSwitch /> : resettable && !onboarding.stepIsRecommended(stepId) && (
        <Button size="xs" variant="ghost" icon={RotateCcw}
          onClick={() => onboarding.resetStep(stepId)}>
          {t('onboarding.actions.resetStep')}
        </Button>
      )}
      bodyClassName="min-h-[340px]"
      footerStart={editing && step > 0 && (
        <Button size="md" variant="ghost" onClick={onboarding.back}>{t('onboarding.actions.back')}</Button>
      )}
      footer={footer}
    >
      <div key={`${stepId}-${phase === 'edit' ? 'edit' : 'apply'}`} className={direction === 'back' ? 'overlay-step-back' : 'overlay-step-forward'}>
        {stepId === 'welcome' && <WelcomeStep onboarding={onboarding} downloadMb={downloadMb} />}
        {stepId === 'features' && <FeaturesStep onboarding={onboarding} />}
        {stepId === 'browserStartup' && <BrowserStartupStep onboarding={onboarding} />}
        {stepId === 'review' && <ReviewStep onboarding={onboarding} required={required} />}
      </div>
    </OverlayShell>
  );
}
